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

// Bridge between the QML view and the stateless faucet core.
//
// This class holds nothing durable. It performs no filesystem access at all —
// no config file, no state file, no cache — because the application it fronts
// writes nothing and must not appear to. Everything it does keep (the kind of
// each live job and the replay copy of a terminal envelope) dies with the
// process, which is exactly the guarantee the core makes about idempotency.
class FaucetBackend : public FaucetBackendSimpleSource,
                      public LogosUiPluginContext
{
    Q_OBJECT

public:
    explicit FaucetBackend(QObject* parent = nullptr);
    ~FaucetBackend() override;

public slots:
    QString bootstrap() override;
    QString startFaucetInfo() override;
    QString startInspectRecipient(QString address) override;
    QString startRequestDrop(QString address, QString requestKey) override;
    QString cancelJob(QString jobId) override;
    QString jobStatus(QString jobId) override;
    QString acknowledgeJob(QString jobId) override;
    QString copyText(QString text) override;

private:
    QString invokeCore(const QString& method, const QVariantList& arguments = {});
    QString startCoreJob(const QString& kind, const QString& method,
                         const QVariantList& arguments = {});
    static QJsonObject parseObject(const QString& json);
    static bool succeeded(const QJsonObject& object);
    static QJsonObject resultObject(const QJsonObject& object);
    static QString jobIdOf(const QJsonObject& envelope);
    // Input bounds are enforced here as well as in QML: this module must not
    // trust its caller, and the pinned base58 decoder underneath can be made
    // to overflow by an unbounded run of leading '1' characters.
    static QString boundedAddress(const QString& address);
    static bool isRequestKey(const QString& requestKey);
    static QString localError(const QString& code, const QString& message);
    static QString localSuccess(const QJsonObject& fields = {});
    void clearTerminalResponse(const QString& jobId);

    // Set once the core has accepted the pinned sequencer. The core rejects
    // reconfiguration after a credit request has started, so a second
    // bootstrap (a view reload) must not attempt one.
    bool m_configured = false;
    QHash<QString, QString> m_jobKinds;
    QHash<QString, QString> m_terminalResponses;
};

#endif
