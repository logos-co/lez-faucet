// SPDX-License-Identifier: MIT OR Apache-2.0
#ifndef LEZ_FAUCET_PLUGIN_H
#define LEZ_FAUCET_PLUGIN_H

#include <QObject>
#include <QString>
#include <QtPlugin>

#include "FaucetPluginInterface.h"
#include "LogosViewPluginBase.h"

class FaucetBackend;
class LogosAPI;

class FaucetPlugin : public QObject,
                     public FaucetPluginInterface,
                     public FaucetBackendViewPluginBase
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID FaucetPluginInterface_iid FILE "../metadata.json")
    Q_INTERFACES(FaucetPluginInterface)

public:
    explicit FaucetPlugin(QObject* parent = nullptr);
    ~FaucetPlugin() override;

    QString name() const override { return QStringLiteral("lez_faucet_ui"); }
    QString version() const override { return QStringLiteral("0.1.0"); }

    Q_INVOKABLE void initLogos(LogosAPI* api);

private:
    FaucetBackend* m_backend = nullptr;
};

#endif
