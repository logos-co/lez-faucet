// SPDX-License-Identifier: MIT OR Apache-2.0
#include "FaucetPlugin.h"

#include "FaucetBackend.h"

FaucetPlugin::FaucetPlugin(QObject* parent)
    : QObject(parent)
{
}

FaucetPlugin::~FaucetPlugin() = default;

void FaucetPlugin::initLogos(LogosAPI* api)
{
    if (m_backend)
        return;

    m_backend = new FaucetBackend(api, this);
    setBackend(m_backend);
}
