// SPDX-License-Identifier: MIT OR Apache-2.0
#ifndef LEZ_FAUCET_PLUGIN_INTERFACE_H
#define LEZ_FAUCET_PLUGIN_INTERFACE_H

#include <QtPlugin>
#include "interface.h"

class FaucetPluginInterface : public PluginInterface
{
public:
    virtual ~FaucetPluginInterface() = default;
};

#define FaucetPluginInterface_iid "org.logos.LEZFaucetPluginInterface"
Q_DECLARE_INTERFACE(FaucetPluginInterface, FaucetPluginInterface_iid)

#endif
