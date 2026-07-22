// SPDX-License-Identifier: MIT OR Apache-2.0
#ifndef LEZ_FAUCET_BACKEND_H
#define LEZ_FAUCET_BACKEND_H

#include <QHash>
#include <QJsonObject>
#include <QJsonValue>
#include <QObject>
#include <QString>
#include <QVariantList>

#include "logos_ui_plugin_context.h"
#include "rep_FaucetBackend_source.h"

class FaucetBackend : public FaucetBackendSimpleSource,
                      public LogosUiPluginContext
{
    Q_OBJECT

public:
    explicit FaucetBackend(QObject* parent = nullptr);
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
    QString acknowledgeJob(QString jobId) override;

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
    static QString localSuccess(const QJsonObject& fields = {});
    void applyProgress(const QString& kind, const QJsonObject& status);
    void applyTerminalResult(const QString& kind, const QJsonObject& status);
    void clearTerminalResponse(const QString& jobId);

    QString m_configPath;
    QString m_storagePath;
    QHash<QString, QString> m_jobKinds;
    QHash<QString, QString> m_terminalResponses;
};

#endif
