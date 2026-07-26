// SPDX-License-Identifier: MIT OR Apache-2.0
#include "FaucetBackend.h"

#include <QChar>
#include <QClipboard>
#include <QGuiApplication>
#include <QJsonDocument>
#include <QJsonValue>

#include "logos_sdk.h"

namespace {

// The one host this build talks to.
//
// There is deliberately no environment variable, no settings entry and no UI
// control that can move it. A redirectable sequencer is not a convenience: it
// lets someone else fabricate the pool balance, the program fingerprint and the
// success receipt, and collect every address typed into this window. The core
// pins the same host independently, so an override here would only ever be
// rejected there.
constexpr auto SEQUENCER_URL = "https://testnet.lez.logos.co";

// A public account id is 32 bytes of base58, so it never exceeds 64
// characters. The bound is applied before the string reaches the decoder.
constexpr int MAX_ACCOUNT_ID_LENGTH = 64;

} // namespace

FaucetBackend::FaucetBackend(QObject* parent)
    : FaucetBackendSimpleSource(parent)
{
    setBusy(false);
    setActiveJobId(QString());
    setSequencerUrl(QString::fromLatin1(SEQUENCER_URL));
}

FaucetBackend::~FaucetBackend()
{
    const auto terminalJobIds = m_terminalResponses.keys();
    for (const QString& jobId : terminalJobIds)
        clearTerminalResponse(jobId);
    if (isContextReady())
        invokeCore(QStringLiteral("shutdown"));
}

QString FaucetBackend::localError(const QString& code, const QString& message)
{
    // Locally generated failures use the same shape as the core's so the view
    // has exactly one error contract to dispatch on, and dispatches on the
    // code rather than on the sentence.
    QJsonObject error;
    error.insert(QStringLiteral("code"), code);
    error.insert(QStringLiteral("message"), message);
    error.insert(QStringLiteral("retryable"), false);

    QJsonObject object;
    object.insert(QStringLiteral("ok"), false);
    object.insert(QStringLiteral("error"), error);
    return QString::fromUtf8(QJsonDocument(object).toJson(QJsonDocument::Compact));
}

QString FaucetBackend::localSuccess(const QJsonObject& fields)
{
    QJsonObject object = fields;
    object.insert(QStringLiteral("ok"), true);
    return QString::fromUtf8(QJsonDocument(object).toJson(QJsonDocument::Compact));
}

QString FaucetBackend::bootstrap()
{
    if (!m_configured) {
        const QString response = invokeCore(QStringLiteral("configure"), {sequencerUrl()});
        if (!succeeded(parseObject(response)))
            return response;
        m_configured = true;
    }

    QJsonObject fields;
    fields.insert(QStringLiteral("sequencer_url"), sequencerUrl());
    // A reloaded view rejoins a live request through this id and recovers its
    // request key from the job envelope. It never mints a new one.
    fields.insert(QStringLiteral("active_job_id"), activeJobId());
    return localSuccess(fields);
}

QString FaucetBackend::invokeCore(const QString& method, const QVariantList& arguments)
{
    if (!isContextReady())
        return localError(QStringLiteral("internal_error"),
                          QStringLiteral("The Logos bridge is not available."));

    // Arguments are never logged. An address is the user's data even though it
    // is public, and the raw envelopes are noise rather than diagnostics.
    logos::CallError error;
    auto& core = modules().lez_faucet;
    QString result;
    if (method == QStringLiteral("configure") && arguments.size() == 1) {
        result = core.configure(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("getFaucetInfo") && arguments.isEmpty()) {
        result = core.getFaucetInfo(&error);
    } else if (method == QStringLiteral("inspectRecipient") && arguments.size() == 1) {
        result = core.inspectRecipient(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("requestDrop") && arguments.size() == 2) {
        result = core.requestDrop(arguments[0].toString(), arguments[1].toString(), &error);
    } else if (method == QStringLiteral("cancel") && arguments.size() == 1) {
        result = core.cancel(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("jobStatus") && arguments.size() == 1) {
        result = core.jobStatus(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("jobResultAck") && arguments.size() == 1) {
        result = core.jobResultAck(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("shutdown") && arguments.isEmpty()) {
        result = core.shutdown(&error);
    } else {
        return localError(QStringLiteral("internal_error"),
                          QStringLiteral("Unsupported core method invocation."));
    }

    if (!error.ok())
        return localError(QStringLiteral("internal_error"), QString::fromStdString(error.message));
    return result;
}

QString FaucetBackend::startCoreJob(
    const QString& kind,
    const QString& method,
    const QVariantList& arguments)
{
    // Only a credit request occupies the single mutating slot. Pool status and
    // recipient inspection are reads and must never queue behind one, or the
    // window looks frozen for the length of a reconciliation and the user
    // force-quits in the middle of a claim.
    if (kind == QStringLiteral("drop") && !activeJobId().isEmpty()) {
        return localError(QStringLiteral("drop_in_progress"),
                          QStringLiteral("A faucet request is already running."));
    }

    const QString response = invokeCore(method, arguments);
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return response;

    const QString jobId = jobIdOf(envelope);
    if (jobId.isEmpty()) {
        // Reported verbatim so the view can decide what to do. For a credit
        // request that decision is to re-call with the *same* request key: a
        // missing id says nothing about whether the job started.
        return localError(QStringLiteral("internal_error"),
                          QStringLiteral("The faucet core did not return a job identifier."));
    }

    m_jobKinds.insert(jobId, kind);
    if (kind == QStringLiteral("drop")) {
        setActiveJobId(jobId);
        setBusy(true);
    }
    return response;
}

QJsonObject FaucetBackend::parseObject(const QString& json)
{
    // Real JSON parsing, never a substring scan: an error message that merely
    // contains the characters "ok":true must not read as a success.
    const QJsonDocument document = QJsonDocument::fromJson(json.toUtf8());
    return document.isObject() ? document.object() : QJsonObject();
}

bool FaucetBackend::succeeded(const QJsonObject& object)
{
    return object.value(QStringLiteral("ok")).toBool(false);
}

QJsonObject FaucetBackend::resultObject(const QJsonObject& object)
{
    const QJsonValue result = object.value(QStringLiteral("result"));
    return result.isObject() ? result.toObject() : object;
}

QString FaucetBackend::jobIdOf(const QJsonObject& envelope)
{
    const QString jobId = envelope.value(QStringLiteral("job_id")).toString();
    if (!jobId.isEmpty())
        return jobId;
    return resultObject(envelope).value(QStringLiteral("job_id")).toString();
}

QString FaucetBackend::boundedAddress(const QString& address)
{
    const QString prefix = QStringLiteral("Public/");
    QString candidate = address.trimmed();
    if (candidate.startsWith(prefix))
        candidate.remove(0, prefix.size());
    else if (candidate.contains(QChar(u'/')))
        return {}; // An explicit prefix check, so Private/... is refused by design.

    candidate = candidate.trimmed();
    if (candidate.isEmpty() || candidate.size() > MAX_ACCOUNT_ID_LENGTH)
        return {};
    for (const QChar character : candidate) {
        // NUL and control characters have no place in an account id and are a
        // way to smuggle a truncated string across the C boundary.
        if (character.unicode() < 0x20 || character.unicode() == 0x7F || character.isSpace())
            return {};
    }
    return candidate;
}

bool FaucetBackend::isRequestKey(const QString& requestKey)
{
    // Mirrors the core's validator exactly: a lowercase RFC 4122 UUID. Keeping
    // the two in step means a malformed key is refused before it can occupy an
    // idempotency slot.
    if (requestKey.size() != 36)
        return false;
    for (int index = 0; index < requestKey.size(); ++index) {
        const QChar character = requestKey.at(index);
        const bool separator = index == 8 || index == 13 || index == 18 || index == 23;
        if (separator) {
            if (character != QChar(u'-'))
                return false;
            continue;
        }
        const char16_t code = character.unicode();
        const bool hexadecimal = (code >= u'0' && code <= u'9') || (code >= u'a' && code <= u'f');
        if (!hexadecimal)
            return false;
    }
    return true;
}

void FaucetBackend::clearTerminalResponse(const QString& jobId)
{
    auto terminal = m_terminalResponses.find(jobId);
    if (terminal == m_terminalResponses.end())
        return;
    m_terminalResponses.erase(terminal);
}

QString FaucetBackend::startFaucetInfo()
{
    return startCoreJob(QStringLiteral("info"), QStringLiteral("getFaucetInfo"));
}

QString FaucetBackend::startInspectRecipient(QString address)
{
    const QString bounded = boundedAddress(address);
    if (bounded.isEmpty()) {
        // The rejected text is never quoted back. It is the user's input, and
        // an error message is not a place to reflect it.
        return localError(QStringLiteral("invalid_public_account_id"),
                          QStringLiteral("Enter a public LEZ account ID."));
    }
    return startCoreJob(QStringLiteral("inspect"), QStringLiteral("inspectRecipient"), {bounded});
}

QString FaucetBackend::startRequestDrop(QString address, QString requestKey)
{
    const QString bounded = boundedAddress(address);
    if (bounded.isEmpty()) {
        return localError(QStringLiteral("invalid_public_account_id"),
                          QStringLiteral("Enter a public LEZ account ID."));
    }
    if (!isRequestKey(requestKey)) {
        return localError(QStringLiteral("invalid_request_key"),
                          QStringLiteral("The request identifier is malformed."));
    }
    return startCoreJob(QStringLiteral("drop"), QStringLiteral("requestDrop"),
                        {bounded, requestKey});
}

QString FaucetBackend::copyText(QString text)
{
    if (text.isEmpty()) {
        return localError(QStringLiteral("internal_error"),
                          QStringLiteral("There is no text to copy."));
    }
    QClipboard* clipboard = QGuiApplication::clipboard();
    if (clipboard == nullptr) {
        return localError(QStringLiteral("internal_error"),
                          QStringLiteral("The system clipboard is not available."));
    }
    clipboard->setText(text);
    return localSuccess();
}

QString FaucetBackend::cancelJob(QString jobId)
{
    if (jobId.isEmpty()) {
        return localError(QStringLiteral("unknown_job"),
                          QStringLiteral("There is no job to cancel."));
    }
    return invokeCore(QStringLiteral("cancel"), {jobId});
}

QString FaucetBackend::jobStatus(QString jobId)
{
    if (jobId.isEmpty()) {
        return localError(QStringLiteral("unknown_job"),
                          QStringLiteral("There is no job to report on."));
    }

    // A terminal envelope is replayed from here until the view acknowledges it,
    // so a connection lost between "succeeded" and the receipt does not lose
    // the proof that the credit landed.
    const auto cached = m_terminalResponses.constFind(jobId);
    if (cached != m_terminalResponses.constEnd())
        return cached.value();

    const QString response = invokeCore(QStringLiteral("jobStatus"), {jobId});
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return response;

    const QString status = envelope.value(QStringLiteral("status")).toString();
    const bool terminal = status == QStringLiteral("succeeded")
        || status == QStringLiteral("failed")
        || status == QStringLiteral("cancelled")
        || status == QStringLiteral("outcome_unknown");
    if (terminal)
        m_terminalResponses.insert(jobId, response);

    return response;
}

QString FaucetBackend::acknowledgeJob(QString jobId)
{
    if (jobId.isEmpty()) {
        return localError(QStringLiteral("unknown_job"),
                          QStringLiteral("There is no job to acknowledge."));
    }

    // The core owns the authoritative result and must release it before this
    // replay copy is discarded, or a failed acknowledgement would leave the
    // view with nothing to show.
    const QString coreResponse = invokeCore(QStringLiteral("jobResultAck"), {jobId});
    const QJsonObject coreEnvelope = parseObject(coreResponse);
    const bool locallyTerminal = m_terminalResponses.contains(jobId);
    const QString coreErrorCode = coreEnvelope.value(QStringLiteral("error")).toObject()
                                      .value(QStringLiteral("code")).toString();
    // A job record the core has already reaped is still an acknowledgement.
    // The request-key tombstone behind it is permanent either way.
    const bool alreadyReaped = locallyTerminal && coreErrorCode == QStringLiteral("unknown_job");
    if (!succeeded(coreEnvelope) && !alreadyReaped)
        return coreResponse;

    const bool known = m_jobKinds.contains(jobId) || locallyTerminal;
    clearTerminalResponse(jobId);
    m_jobKinds.remove(jobId);
    if (activeJobId() == jobId) {
        setActiveJobId(QString());
        setBusy(false);
    }

    QJsonObject fields;
    fields.insert(QStringLiteral("job_id"), jobId);
    fields.insert(QStringLiteral("acknowledged"), known);
    return localSuccess(fields);
}
