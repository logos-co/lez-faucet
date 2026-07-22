// SPDX-License-Identifier: MIT OR Apache-2.0
#ifndef LEZ_FAUCET_BACKEND_H
#define LEZ_FAUCET_BACKEND_H

#include <QHash>
#include <QJsonObject>
#include <QJsonValue>
#include <QObject>
#include <QString>
#include <QVariantList>

#include "rep_FaucetBackend_source.h"

class LogosAPI;

class FaucetBackend : public FaucetBackendSimpleSource
{
    Q_OBJECT

public:
    explicit FaucetBackend(LogosAPI* logosAPI, QObject* parent = nullptr);
    ~FaucetBackend() override;

public slots:
    QString bootstrap() override;
    QString startCreate(QString password) override;
    QString startOpen() override;
    QString startVerifyFingerprint() override;
    QString startInitializeAccount() override;
    QString startBalance() override;
    QString startClaimOnce() override;
    QString startClaimUntilTarget(QString target, int maxClaims) override;
    QString cancelJob(QString jobId) override;
    QString jobStatus(QString jobId) override;

private:
    QString invokeCore(const QString& method, const QVariantList& arguments = {});
    QString startCoreJob(const QString& kind, const QString& method,
                         const QVariantList& arguments = {});
    static QJsonObject parseObject(const QString& json);
    static bool succeeded(const QJsonObject& object);
    static QJsonObject resultObject(const QJsonObject& object);
    static QJsonObject statusPayload(const QJsonObject& object);
    static QJsonValue completedResult(const QJsonObject& object);
    static QString scalarString(const QJsonValue& value);
    static QString localError(const QString& message);
    void applyTerminalResult(const QString& kind, const QJsonObject& status);

    LogosAPI* m_logosAPI = nullptr;
    QString m_configPath;
    QString m_storagePath;
    QHash<QString, QString> m_jobKinds;
};

#endif
