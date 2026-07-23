// SPDX-License-Identifier: MIT OR Apache-2.0
#include "FaucetBackend.h"

#include <QDir>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonValue>
#include <QStandardPaths>
#include <QtGlobal>

#include "logos_sdk.h"

namespace {
constexpr auto DEFAULT_SEQUENCER_URL = "https://testnet.lez.logos.co";
constexpr int MAX_CLAIMS_PER_RUN = 100;
}

FaucetBackend::FaucetBackend(QObject* parent)
    : FaucetBackendSimpleSource(parent)
{
    const QString dataRoot = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation)
        + QStringLiteral("/lez-faucet");
    m_configPath = dataRoot + QStringLiteral("/wallet_config.json");
    m_storagePath = dataRoot + QStringLiteral("/wallet.json");

    setWalletExists(QFileInfo::exists(m_storagePath));
    setBusy(false);
    setAccountId(QString());
    setBalance(QString());
    setLastTxHash(QString());
    QString configuredSequencer = qEnvironmentVariable("LEZ_FAUCET_SEQUENCER_URL").trimmed();
    if (configuredSequencer.isEmpty())
        configuredSequencer = QString::fromLatin1(DEFAULT_SEQUENCER_URL);
    setSequencerUrl(configuredSequencer);
    // The pinned Rust surface has no mnemonic-restoration function in v0.1.
    setRecoverySupported(false);
    setActiveJobId(QString());
    setActiveJobKind(QString());
    setActiveRecipientId(QString());
}

FaucetBackend::~FaucetBackend()
{
    const auto terminalJobIds = m_terminalResponses.keys();
    for (const QString& jobId : terminalJobIds)
        clearTerminalResponse(jobId);
    if (isContextReady())
        invokeCore(QStringLiteral("destroy"));
}

QString FaucetBackend::localError(const QString& message)
{
    QJsonObject object;
    object.insert(QStringLiteral("ok"), false);
    object.insert(QStringLiteral("error"), message);
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
    QJsonObject object;
    object.insert(QStringLiteral("ok"), true);
    object.insert(QStringLiteral("wallet_exists"), walletExists());
    object.insert(QStringLiteral("account_id"), accountId());
    object.insert(QStringLiteral("recovery_supported"), recoverySupported());
    object.insert(QStringLiteral("sequencer_url"), sequencerUrl());
    object.insert(QStringLiteral("active_job_id"), activeJobId());
    object.insert(QStringLiteral("active_job_kind"), activeJobKind());
    object.insert(QStringLiteral("active_recipient_id"), activeRecipientId());
    return QString::fromUtf8(QJsonDocument(object).toJson(QJsonDocument::Compact));
}

QString FaucetBackend::invokeCore(const QString& method, const QVariantList& arguments)
{
    if (!isContextReady())
        return localError(QStringLiteral("Logos bridge is not available"));

    // Never log method arguments or raw results: create carries a password in,
    // and its one-shot result carries the mnemonic out.
    logos::CallError error;
    auto& core = modules().lez_faucet;
    QString result;
    if (method == QStringLiteral("create") && arguments.size() == 4) {
        result = core.create(arguments[0].toString(), arguments[1].toString(),
                             arguments[2].toString(), arguments[3].toString(), &error);
    } else if (method == QStringLiteral("open") && arguments.size() == 3) {
        result = core.open(arguments[0].toString(), arguments[1].toString(),
                           arguments[2].toString(), &error);
    } else if (method == QStringLiteral("destroy") && arguments.isEmpty()) {
        result = core.destroy(&error);
    } else if (method == QStringLiteral("verifyFingerprint") && arguments.isEmpty()) {
        result = core.verifyFingerprint(&error);
    } else if (method == QStringLiteral("createAndInitializeAccount") && arguments.isEmpty()) {
        result = core.createAndInitializeAccount(&error);
    } else if (method == QStringLiteral("balance") && arguments.size() == 1) {
        result = core.balance(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("claimOnce") && arguments.size() == 1) {
        result = core.claimOnce(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("claimUntilTarget") && arguments.size() == 3) {
        result = core.claimUntilTarget(arguments[0].toString(), arguments[1].toString(),
                                       arguments[2].toInt(), &error);
    } else if (method == QStringLiteral("cancel") && arguments.size() == 1) {
        result = core.cancel(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("jobStatus") && arguments.size() == 1) {
        result = core.jobStatus(arguments[0].toString(), &error);
    } else if (method == QStringLiteral("jobResultAck") && arguments.size() == 1) {
        result = core.jobResultAck(arguments[0].toString(), &error);
    } else {
        return localError(QStringLiteral("Unsupported core method invocation"));
    }

    if (!error.ok())
        return localError(QString::fromStdString(error.message));
    return result;
}

QString FaucetBackend::startCoreJob(
    const QString& kind,
    const QString& method,
    const QVariantList& arguments)
{
    if (!activeJobId().isEmpty()) {
        return localError(QStringLiteral("Another faucet operation is still active: %1")
                              .arg(activeJobId()));
    }

    const QString response = invokeCore(method, arguments);
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return response;

    const QJsonObject result = resultObject(envelope);
    QString jobId = envelope.value(QStringLiteral("job_id")).toString();
    if (jobId.isEmpty())
        jobId = result.value(QStringLiteral("job_id")).toString();
    if (jobId.isEmpty())
        return localError(QStringLiteral("Core operation did not return a job ID"));

    m_jobKinds.insert(jobId, kind);
    setActiveJobId(jobId);
    setActiveJobKind(kind);
    setBusy(true);
    return response;
}

QJsonObject FaucetBackend::parseObject(const QString& json)
{
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

QJsonObject FaucetBackend::statusPayload(const QJsonObject& object)
{
    return object;
}

QJsonValue FaucetBackend::completedResult(const QJsonObject& object)
{
    const QJsonValue nativeResult = object.value(QStringLiteral("result"));
    if (!nativeResult.isObject())
        return nativeResult;

    const QJsonObject wrapper = nativeResult.toObject();
    const QJsonValue operationResult = wrapper.value(QStringLiteral("result"));
    if (wrapper.value(QStringLiteral("ok")).toBool(false) && !operationResult.isUndefined())
        return operationResult;
    return nativeResult;
}

QString FaucetBackend::scalarString(const QJsonValue& value)
{
    if (value.isString())
        return value.toString();
    return QString();
}

QString FaucetBackend::normalizedPublicAccountId(const QString& accountId)
{
    QString normalized = accountId.trimmed();
    if (normalized.startsWith(QStringLiteral("Public/")))
        normalized.remove(0, 7);
    else if (normalized.contains(QChar(u'/')))
        return {};
    return normalized.trimmed();
}

QString FaucetBackend::startedJobId(const QString& response)
{
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return {};
    QString jobId = envelope.value(QStringLiteral("job_id")).toString();
    if (jobId.isEmpty())
        jobId = resultObject(envelope).value(QStringLiteral("job_id")).toString();
    return jobId;
}

QString FaucetBackend::startExternalJob(
    const QString& kind,
    const QString& method,
    const QString& accountId,
    const QVariantList& remainingArguments)
{
    if (!m_clientOpen) {
        return localError(QStringLiteral(
            "Open or create the local faucet wallet before funding an existing account"));
    }
    const QString normalized = normalizedPublicAccountId(accountId);
    if (normalized.isEmpty()) {
        return localError(QStringLiteral(
            "Enter a public account ID as Public/<account-id> or a bare account ID"));
    }

    QVariantList arguments{normalized};
    arguments.append(remainingArguments);
    const QString response = startCoreJob(kind, method, arguments);
    const QString jobId = startedJobId(response);
    if (!jobId.isEmpty()) {
        m_jobRecipients.insert(jobId, normalized);
        setActiveRecipientId(normalized);
    }
    return response;
}

void FaucetBackend::applyTerminalResult(const QString& kind, const QJsonObject& status)
{
    const QJsonValue resultValue = completedResult(status);
    const QJsonObject result = resultValue.isObject() ? resultValue.toObject() : QJsonObject();

    if (kind == QStringLiteral("create")) {
        m_clientOpen = true;
        setWalletExists(true);
        setAccountId(QString());
        setBalance(QString());
        return;
    }

    if (kind == QStringLiteral("open")) {
        m_clientOpen = true;
        return;
    }

    if (kind == QStringLiteral("initialize")) {
        const QString nextAccountId = result.value(QStringLiteral("account_id")).toString();
        if (!nextAccountId.isEmpty())
            setAccountId(nextAccountId);
        const QString nextBalance = scalarString(result.value(QStringLiteral("balance")));
        if (!nextBalance.isEmpty())
            setBalance(nextBalance);
        const QString txHash = result.value(QStringLiteral("init_tx_hash")).toString();
        if (!txHash.isEmpty())
            setLastTxHash(txHash);
        return;
    }

    if (kind == QStringLiteral("balance")) {
        const QString nextBalance = scalarString(resultValue);
        if (!nextBalance.isEmpty())
            setBalance(nextBalance);
        return;
    }

    if (kind == QStringLiteral("claim_once")) {
        const QString nextBalance = scalarString(result.value(QStringLiteral("balance_after")));
        if (!nextBalance.isEmpty())
            setBalance(nextBalance);
        const QString txHash = result.value(QStringLiteral("tx_hash")).toString();
        if (!txHash.isEmpty())
            setLastTxHash(txHash);
        return;
    }

    if (kind == QStringLiteral("claim_target")) {
        const QString nextBalance = scalarString(result.value(QStringLiteral("final_balance")));
        if (!nextBalance.isEmpty())
            setBalance(nextBalance);
    }
}

void FaucetBackend::applyProgress(const QString& kind, const QJsonObject& status)
{
    if (kind != QStringLiteral("claim_target"))
        return;

    const QJsonObject progress = status.value(QStringLiteral("progress")).toObject();
    const QString nextBalance = scalarString(progress.value(QStringLiteral("balance")));
    if (!nextBalance.isEmpty())
        setBalance(nextBalance);
    const QString txHash = progress.value(QStringLiteral("receipt")).toObject()
                               .value(QStringLiteral("tx_hash")).toString();
    if (!txHash.isEmpty())
        setLastTxHash(txHash);
}

void FaucetBackend::clearTerminalResponse(const QString& jobId)
{
    auto terminal = m_terminalResponses.find(jobId);
    if (terminal == m_terminalResponses.end())
        return;
    terminal.value().fill(QChar(u'\0'));
    m_terminalResponses.erase(terminal);
}

QString FaucetBackend::startCreate(QString password)
{
    if (password.isEmpty())
        return localError(QStringLiteral("Wallet password cannot be empty"));

    QDir().mkpath(QFileInfo(m_storagePath).absolutePath());
    const QString result = startCoreJob(
        QStringLiteral("create"),
        QStringLiteral("create"),
        {m_configPath, m_storagePath, sequencerUrl(), password});
    password.fill(QChar(u'\0'));
    return result;
}

QString FaucetBackend::startOpen()
{
    if (!QFileInfo::exists(m_storagePath))
        return localError(QStringLiteral("Wallet file does not exist"));

    return startCoreJob(
        QStringLiteral("open"),
        QStringLiteral("open"),
        {m_configPath, m_storagePath, sequencerUrl()});
}

QString FaucetBackend::startVerifyFingerprint()
{
    return startCoreJob(
        QStringLiteral("verify"),
        QStringLiteral("verifyFingerprint"));
}

QString FaucetBackend::startInitializeAccount()
{
    return startCoreJob(
        QStringLiteral("initialize"),
        QStringLiteral("createAndInitializeAccount"));
}

QString FaucetBackend::startBalance()
{
    if (accountId().isEmpty())
        return localError(QStringLiteral("No initialized public account is selected"));

    return startCoreJob(
        QStringLiteral("balance"),
        QStringLiteral("balance"),
        {accountId()});
}

QString FaucetBackend::startClaimOnce()
{
    if (accountId().isEmpty())
        return localError(QStringLiteral("No initialized public account is selected"));

    return startCoreJob(
        QStringLiteral("claim_once"),
        QStringLiteral("claimOnce"),
        {accountId()});
}

QString FaucetBackend::startClaimUntilTarget(QString target)
{
    if (accountId().isEmpty())
        return localError(QStringLiteral("No initialized public account is selected"));

    return startCoreJob(
        QStringLiteral("claim_target"),
        QStringLiteral("claimUntilTarget"),
        {accountId(), target, MAX_CLAIMS_PER_RUN});
}

QString FaucetBackend::startExternalBalance(QString accountId)
{
    if (!activeJobId().isEmpty()) {
        return localError(QStringLiteral("Another faucet operation is still active: %1")
                              .arg(activeJobId()));
    }
    m_verifiedExternalRecipient.clear();
    return startExternalJob(
        QStringLiteral("external_balance"),
        QStringLiteral("balance"),
        accountId);
}

QString FaucetBackend::startExternalClaimOnce(QString accountId)
{
    const QString normalized = normalizedPublicAccountId(accountId);
    if (normalized.isEmpty() || normalized != m_verifiedExternalRecipient) {
        return localError(QStringLiteral(
            "Check this public account and its balance before claiming"));
    }
    return startExternalJob(
        QStringLiteral("external_claim_once"),
        QStringLiteral("claimOnce"),
        normalized);
}

QString FaucetBackend::startExternalClaimUntilTarget(QString accountId, QString target)
{
    const QString normalized = normalizedPublicAccountId(accountId);
    if (normalized.isEmpty() || normalized != m_verifiedExternalRecipient) {
        return localError(QStringLiteral(
            "Check this public account and its balance before claiming"));
    }
    return startExternalJob(
        QStringLiteral("external_claim_target"),
        QStringLiteral("claimUntilTarget"),
        normalized,
        {target, MAX_CLAIMS_PER_RUN});
}

QString FaucetBackend::cancelJob(QString jobId)
{
    if (jobId.isEmpty())
        return localError(QStringLiteral("Job ID cannot be empty"));
    return invokeCore(QStringLiteral("cancel"), {jobId});
}

QString FaucetBackend::jobStatus(QString jobId)
{
    if (jobId.isEmpty())
        return localError(QStringLiteral("Job ID cannot be empty"));

    const auto cached = m_terminalResponses.constFind(jobId);
    if (cached != m_terminalResponses.constEnd())
        return cached.value();

    const QString response = invokeCore(QStringLiteral("jobStatus"), {jobId});
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return response;

    const QJsonObject payload = statusPayload(envelope);
    const QString state = payload.value(QStringLiteral("status")).toString().toLower();
    const QString kind = m_jobKinds.value(jobId);
    applyProgress(kind, envelope);
    const bool terminal = state == QStringLiteral("completed")
        || state == QStringLiteral("failed")
        || state == QStringLiteral("cancelled");
    if (terminal) {
        if (state == QStringLiteral("completed")) {
            applyTerminalResult(kind, envelope);
            if (kind == QStringLiteral("external_balance"))
                m_verifiedExternalRecipient = m_jobRecipients.value(jobId);
        }
        m_terminalResponses.insert(jobId, response);
    }

    return response;
}

QString FaucetBackend::acknowledgeJob(QString jobId)
{
    if (jobId.isEmpty())
        return localError(QStringLiteral("Job ID cannot be empty"));

    // The core owns the authoritative sensitive result. It must acknowledge
    // and clear that result before this UI-side replay cache can be discarded.
    const QString coreResponse = invokeCore(QStringLiteral("jobResultAck"), {jobId});
    const QJsonObject coreEnvelope = parseObject(coreResponse);
    const bool locallyTerminal = m_terminalResponses.contains(jobId);
    const QString coreErrorCode = coreEnvelope.value(QStringLiteral("error")).toObject()
                                      .value(QStringLiteral("code")).toString();
    const bool alreadyReaped = locallyTerminal && coreErrorCode == QStringLiteral("unknown_job");
    if (!succeeded(coreEnvelope) && !alreadyReaped)
        return coreResponse;

    const bool known = m_jobKinds.contains(jobId) || m_terminalResponses.contains(jobId);
    clearTerminalResponse(jobId);
    m_jobKinds.remove(jobId);
    m_jobRecipients.remove(jobId);
    if (activeJobId() == jobId) {
        setActiveJobId(QString());
        setActiveJobKind(QString());
        setActiveRecipientId(QString());
    }
    setBusy(!activeJobId().isEmpty());

    QJsonObject fields;
    fields.insert(QStringLiteral("job_id"), jobId);
    fields.insert(QStringLiteral("acknowledged"), known);
    return localSuccess(fields);
}
