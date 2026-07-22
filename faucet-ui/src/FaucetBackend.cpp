// SPDX-License-Identifier: MIT OR Apache-2.0
#include "FaucetBackend.h"

#include <QDir>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonValue>
#include <QSettings>
#include <QStandardPaths>

#include "logos_api.h"
#include "logos_api_client.h"

namespace {
constexpr auto CORE_MODULE = "lez_faucet";
constexpr auto SETTINGS_ORG = "Logos";
constexpr auto SETTINGS_APP = "LEZFaucetUI";
constexpr auto ACCOUNT_ID_KEY = "publicAccountId";
const Timeout NO_TIMEOUT{-1};
}

FaucetBackend::FaucetBackend(LogosAPI* logosAPI, QObject* parent)
    : FaucetBackendSimpleSource(parent),
      m_logosAPI(logosAPI)
{
    const QString dataRoot = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation)
        + QStringLiteral("/lez-faucet");
    m_configPath = dataRoot + QStringLiteral("/wallet_config.json");
    m_storagePath = dataRoot + QStringLiteral("/wallet.json");

    setWalletExists(QFileInfo::exists(m_storagePath));
    setBusy(false);
    setAccountId(QSettings(SETTINGS_ORG, SETTINGS_APP).value(ACCOUNT_ID_KEY).toString());
    setBalance(QString());
    setLastTxHash(QString());
    setSequencerUrl(QStringLiteral("https://testnet.lez.logos.co"));
    // The pinned Rust surface has no mnemonic-restoration function in v0.1.
    setRecoverySupported(false);
}

FaucetBackend::~FaucetBackend()
{
    if (m_logosAPI)
        invokeCore(QStringLiteral("destroy"));
}

QString FaucetBackend::localError(const QString& message)
{
    QJsonObject object;
    object.insert(QStringLiteral("ok"), false);
    object.insert(QStringLiteral("error"), message);
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
    return QString::fromUtf8(QJsonDocument(object).toJson(QJsonDocument::Compact));
}

QString FaucetBackend::invokeCore(const QString& method, const QVariantList& arguments)
{
    if (!m_logosAPI)
        return localError(QStringLiteral("Logos bridge is not available"));

    auto* client = m_logosAPI->getClient(QLatin1String(CORE_MODULE));
    if (!client)
        return localError(QStringLiteral("LEZ Faucet core module is not available"));

    // Never log method arguments or raw results: create carries a password in,
    // and its one-shot result carries the mnemonic out.
    return client->invokeRemoteMethod(
        QLatin1String(CORE_MODULE), method, arguments, NO_TIMEOUT).toString();
}

QString FaucetBackend::startCoreJob(
    const QString& kind,
    const QString& method,
    const QVariantList& arguments)
{
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
    if (value.isDouble())
        return QString::number(value.toDouble(), 'f', 0);
    return value.toVariant().toString();
}

void FaucetBackend::applyTerminalResult(const QString& kind, const QJsonObject& status)
{
    const QJsonValue resultValue = completedResult(status);
    const QJsonObject result = resultValue.isObject() ? resultValue.toObject() : QJsonObject();

    if (kind == QStringLiteral("create")) {
        setWalletExists(true);
        setAccountId(QString());
        setBalance(QString());
        QSettings(SETTINGS_ORG, SETTINGS_APP).remove(ACCOUNT_ID_KEY);
        return;
    }

    if (kind == QStringLiteral("initialize")) {
        const QString nextAccountId = result.value(QStringLiteral("account_id")).toString();
        if (!nextAccountId.isEmpty()) {
            setAccountId(nextAccountId);
            QSettings(SETTINGS_ORG, SETTINGS_APP).setValue(ACCOUNT_ID_KEY, nextAccountId);
        }
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
        const QJsonArray claims = result.value(QStringLiteral("claims")).toArray();
        if (!claims.isEmpty()) {
            const QString txHash = claims.last().toObject().value(QStringLiteral("tx_hash")).toString();
            if (!txHash.isEmpty())
                setLastTxHash(txHash);
        }
    }
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

QString FaucetBackend::startClaimUntilTarget(QString target, int maxClaims)
{
    if (accountId().isEmpty())
        return localError(QStringLiteral("No initialized public account is selected"));
    if (maxClaims <= 0)
        return localError(QStringLiteral("Claim limit must be positive"));

    return startCoreJob(
        QStringLiteral("claim_target"),
        QStringLiteral("claimUntilTarget"),
        {accountId(), target, maxClaims});
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

    const QString response = invokeCore(QStringLiteral("jobStatus"), {jobId});
    const QJsonObject envelope = parseObject(response);
    if (!succeeded(envelope))
        return response;

    const QJsonObject payload = statusPayload(envelope);
    const QString state = payload.value(QStringLiteral("status")).toString().toLower();
    const bool terminal = state == QStringLiteral("completed")
        || state == QStringLiteral("failed")
        || state == QStringLiteral("cancelled");
    if (terminal) {
        const QString kind = m_jobKinds.take(jobId);
        if (state == QStringLiteral("completed"))
            applyTerminalResult(kind, envelope);
        setBusy(!m_jobKinds.isEmpty());
    } else if (m_jobKinds.value(jobId) == QStringLiteral("claim_target")) {
        const QJsonObject progress = payload.value(QStringLiteral("progress")).toObject();
        const QString nextBalance = scalarString(progress.value(QStringLiteral("balance")));
        if (!nextBalance.isEmpty())
            setBalance(nextBalance);
    }

    return response;
}
