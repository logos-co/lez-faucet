// SPDX-License-Identifier: MIT OR Apache-2.0
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Logos.Theme
import Logos.Controls
import "FaucetFlow.js" as FaucetFlow

Rectangle {
    id: root
    color: Theme.palette.background

    readonly property var backend: typeof logos !== "undefined" && logos.module
        ? logos.module("lez_faucet_ui") : null
    property bool backendReady: false
    property string screenState: "booting"
    property string resumeState: "welcome"
    property string statusText: "Starting LEZ Faucet…"
    property string errorText: ""
    property string technicalDetails: ""
    property string mnemonicText: ""
    property string mnemonicJobId: ""
    property string targetText: "1000"
    property int completedClaims: 0
    property int requiredClaims: 0
    property bool stopRequested: false
    property bool cancelRequestPending: false
    property string activeJobId: ""
    property string activeJobKind: ""
    property string activeJobResumeState: "welcome"
    property bool jobPollInFlight: false

    readonly property string accountId: backend ? backend.accountId : ""
    readonly property string balanceText: backend && backend.balance !== "" ? backend.balance : "0"
    readonly property bool hasAccount: accountId !== ""

    function watch(reply, onSuccess, onError) {
        if (typeof logos === "undefined" || !logos.watch) {
            onError("Logos bridge is not available")
            return
        }
        logos.watch(reply,
            function(value) { onSuccess(value) },
            function(error) { onError(String(error || "Unknown backend error")) })
    }

    function parseEnvelope(raw) {
        try {
            var object = JSON.parse(String(raw || "{}"))
            if (!object.ok)
                return { ok: false, error: errorMessage(object.error) }
            return object
        } catch (error) {
            return { ok: false, error: "The faucet returned an invalid response" }
        }
    }

    function resultObject(envelope) {
        return envelope && envelope.result !== undefined ? envelope.result : envelope
    }

    function errorMessage(error) {
        if (!error)
            return "Operation failed"
        if (typeof error === "string")
            return error
        if (error.message)
            return error.code ? String(error.message) + " (" + String(error.code) + ")" : String(error.message)
        return "Operation failed"
    }

    function routeError(message, safeResumeState) {
        var category = FaucetFlow.classifyJobError(message)
        errorText = errorMessage(message)
        technicalDetails = typeof message === "object" ? JSON.stringify(message) : errorText
        if (category === "version_mismatch") {
            screenState = "version_mismatch"
        } else if (category === "outcome_unknown") {
            resumeState = "ready"
            screenState = "outcome_unknown"
        } else if (category === "stale") {
            resumeState = safeResumeState || "ready"
            screenState = "stale"
        } else if (category === "offline") {
            resumeState = safeResumeState || (hasAccount ? "ready" : "welcome")
            screenState = "offline"
        } else {
            screenState = "error"
        }
    }

    function beginBootstrap() {
        if (!backendReady || !backend)
            return
        screenState = "booting"
        statusText = "Starting LEZ Faucet…"
        watch(backend.bootstrap(), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                routeError(envelope.error, "welcome")
                return
            }
            var pendingJobId = String(envelope.active_job_id || backend.activeJobId || "")
            var pendingJobKind = String(envelope.active_job_kind || backend.activeJobKind || "")
            if (pendingJobId !== "" && pendingJobKind !== "") {
                resumeJob(pendingJobId, pendingJobKind)
                return
            }
            if (envelope.wallet_exists)
                openExistingWallet()
            else
                screenState = "welcome"
        }, function(error) { routeError(error, "welcome") })
    }

    function resumeJob(jobId, kind) {
        var screens = {
            "create": "creating", "open": "opening", "verify": "compatibility",
            "initialize": "initializing", "balance": "booting",
            "claim_once": "claiming_once", "claim_target": "claiming_target"
        }
        activeJobId = jobId
        activeJobKind = kind
        activeJobResumeState = kind === "create" ? "welcome"
            : (kind === "initialize" ? "initialization_required" : "ready")
        screenState = screens[kind] || "booting"
        statusText = "Reconnecting to the in-progress faucet operation…"
        jobPollTimer.start()
        pollActiveJob()
    }

    function openExistingWallet() {
        screenState = "opening"
        statusText = "Opening your testnet account…"
        watch(backend.startOpen(), function(raw) {
            startJob(raw, "open", "open_error")
        }, function(error) {
            errorText = error
            screenState = "open_error"
        })
    }

    function verifyCompatibility(nextState) {
        screenState = "compatibility"
        statusText = "Checking testnet compatibility…"
        watch(backend.startVerifyFingerprint(), function(raw) {
            startJob(raw, "verify", nextState)
        }, function(error) { routeError(error, nextState) })
    }

    function submitCreate(password) {
        screenState = "creating"
        statusText = "Creating wallet…"
        watch(backend.startCreate(password), function(raw) {
            startJob(raw, "create", "welcome")
        }, function(error) { routeError(error, "welcome") })
    }

    function acknowledgeMnemonic() {
        if (mnemonicJobId === "")
            return
        var acknowledgedJobId = mnemonicJobId
        watch(backend.acknowledgeJob(acknowledgedJobId), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                errorText = envelope.error
                return
            }
            mnemonicText = ""
            mnemonicJobId = ""
            mnemonicAcknowledged.checked = false
            verifyCompatibility("initialization_required")
        }, function(error) { errorText = error })
    }

    function initializeAccount(successState) {
        screenState = "initializing"
        statusText = "Initializing account on testnet…"
        watch(backend.startInitializeAccount(), function(raw) {
            startJob(raw, "initialize", successState || "initialization_required")
        }, function(error) { routeError(error, "initialization_required") })
    }

    function refreshBalance() {
        if (!hasAccount) {
            screenState = "initialization_required"
            return
        }
        watch(backend.startBalance(), function(raw) {
            startJob(raw, "balance", "ready")
        }, function(error) { routeError(error, "ready") })
    }

    function startClaimOnce() {
        screenState = "claiming_once"
        statusText = "Fetching the current faucet challenge…"
        watch(backend.startClaimOnce(), function(raw) {
            startJob(raw, "claim_once", "ready")
        }, function(error) { routeError(error, "ready") })
    }

    function startClaimToTarget() {
        if (!FaucetFlow.targetPending(balanceText, targetText))
            return
        requiredClaims = 0
        completedClaims = 0
        stopRequested = false
        screenState = "claiming_target"
        statusText = "Fetching the current faucet challenge…"
        watch(backend.startClaimUntilTarget(targetText), function(raw) {
            startJob(raw, "claim_target", "ready")
        }, function(error) { routeError(error, "ready") })
    }

    function startJob(raw, kind, safeResumeState) {
        var envelope = parseEnvelope(raw)
        if (!envelope.ok) {
            routeError(envelope.error, safeResumeState)
            return
        }
        var started = resultObject(envelope)
        var jobId = String(envelope.job_id || (started && started.job_id) || "")
        if (jobId === "") {
            routeError("The faucet did not return a job ID", safeResumeState)
            return
        }
        activeJobId = jobId
        activeJobKind = kind
        activeJobResumeState = safeResumeState
        jobPollTimer.start()
        pollActiveJob()
    }

    function normalizeOperationResult(value) {
        var normalized = value
        if (typeof normalized === "string") {
            try { normalized = JSON.parse(normalized) }
            catch (error) { return { ok: false, error: "The job returned an invalid result" } }
        }
        if (normalized && normalized.ok === false)
            return normalized
        return normalized && normalized.ok === true ? resultObject(normalized) : normalized
    }

    function clearActiveJob() {
        jobPollTimer.stop()
        activeJobId = ""
        activeJobKind = ""
        activeJobResumeState = "welcome"
        jobPollInFlight = false
    }

    function pauseActiveJob() {
        jobPollTimer.stop()
        jobPollInFlight = false
    }

    function acknowledgeTerminalJob(jobId, onAcknowledged) {
        watch(backend.acknowledgeJob(jobId), function(raw) {
            var envelope = parseEnvelope(raw)
            if (FaucetFlow.ackDisposition(envelope) !== "clear") {
                statusText = "Could not acknowledge the result; reconnecting…"
                jobPollTimer.start()
                return
            }
            clearActiveJob()
            onAcknowledged()
        }, function(error) {
            statusText = "Connection interrupted while acknowledging the result; reconnecting…"
            jobPollTimer.start()
        })
    }

    function pollActiveJob() {
        if (activeJobId === "" || jobPollInFlight)
            return
        jobPollInFlight = true
        var polledJobId = activeJobId
        watch(backend.jobStatus(polledJobId), function(raw) {
            jobPollInFlight = false
            if (polledJobId !== activeJobId)
                return
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                statusText = "Connection interrupted; retrying this operation…"
                errorText = envelope.error
                return
            }
            var payload = envelope
            var update = FaucetFlow.reduceJobEnvelope(
                payload, completedClaims, requiredClaims, balanceText)
            var state = update.state
            completedClaims = update.completedClaims
            requiredClaims = update.requiredClaims
            if (payload.phase)
                statusText = String(payload.phase)
            else if (activeJobKind === "claim_once" || activeJobKind === "claim_target")
                statusText = "Computing a solution and waiting for confirmation…"

            if (!update.terminal)
                return

            var finishedKind = activeJobKind
            var finishedResume = activeJobResumeState
            var operationResult = normalizeOperationResult(payload.result)
            pauseActiveJob()

            if (state === "completed" && finishedKind === "create") {
                mnemonicJobId = polledJobId
                clearActiveJob()
                handleJobSuccess(finishedKind, operationResult || {}, finishedResume)
                return
            }

            acknowledgeTerminalJob(polledJobId, function() {
                cancelRequestPending = false
                if (state === "cancelled") {
                    screenState = "ready"
                    statusText = completedClaims > 0
                        ? qsTr("Stopped after %1 confirmed claim(s)").arg(completedClaims)
                        : qsTr("Stopped before another claim was submitted")
                    refreshBalance()
                    return
                }
                if (state !== "completed") {
                    routeError(payload.error || (operationResult && operationResult.error), finishedResume)
                    return
                }
                if (operationResult && operationResult.ok === false) {
                    routeError(operationResult.error, finishedResume)
                    return
                }
                handleJobSuccess(finishedKind, operationResult || {}, finishedResume)
            })
        }, function(error) {
            jobPollInFlight = false
            errorText = String(error || "Connection interrupted")
            statusText = "Connection interrupted; retrying this operation…"
        })
    }

    function handleJobSuccess(kind, result, successState) {
        if (kind === "create") {
            // The backend/core replay this terminal result until the user
            // explicitly acknowledges it; no mnemonic is exposed as a property.
            mnemonicText = String(result.mnemonic || "")
            if (mnemonicText === "") {
                routeError("Wallet created without a recovery phrase", "initialization_required")
                return
            }
            screenState = "mnemonic"
        } else if (kind === "open") {
            verifyCompatibility("resume_account")
        } else if (kind === "verify") {
            technicalDetails = JSON.stringify(result)
            if (successState === "resume_account")
                initializeAccount("ready")
            else if (successState === "ready")
                refreshBalance()
            else
                screenState = successState
        } else if (kind === "initialize") {
            screenState = "ready"
            statusText = "Your testnet account is ready"
        } else if (kind === "balance") {
            screenState = "ready"
            statusText = "Your testnet account is ready"
        } else if (kind === "claim_once") {
            screenState = "ready"
            statusText = Number(result.stale_challenge_retries || 0) > 0
                ? "150 LEZ claimed after refreshing a stale challenge" : "150 LEZ claimed"
        } else if (kind === "claim_target") {
            screenState = "ready"
            statusText = "Target reached"
        }
    }

    function requestStop() {
        if (activeJobId !== "") {
            cancelRequestPending = true
            screenState = "cancelling"
            statusText = "Stopping… A submitted claim cannot be cancelled."
            watch(backend.cancelJob(activeJobId), function(raw) {
                var envelope = parseEnvelope(raw)
                cancelRequestPending = false
                if (!envelope.ok) {
                    errorText = envelope.error
                    screenState = "claiming_target"
                    statusText = "Could not request stop; claim-to-target continues."
                    return
                }
                if (FaucetFlow.cancelAcknowledged(envelope)) {
                    stopRequested = true
                    statusText = "Stop requested. Waiting for any submitted claim to settle…"
                } else {
                    screenState = "claiming_target"
                    statusText = "The faucet did not acknowledge the stop request; claim-to-target continues."
                }
            }, function(error) {
                cancelRequestPending = false
                errorText = error
                screenState = "claiming_target"
                statusText = "Could not request stop; claim-to-target continues."
            })
        } else {
            screenState = "ready"
            statusText = "Stopped"
        }
    }

    function retryFromOffline() {
        if (resumeState === "ready")
            verifyCompatibility("ready")
        else if (resumeState === "initialization_required")
            verifyCompatibility("initialization_required")
        else
            beginBootstrap()
    }

    Component.onDestruction: mnemonicText = ""

    Timer {
        id: jobPollTimer
        interval: 500
        repeat: true
        running: false
        onTriggered: root.pollActiveJob()
    }

    Timer {
        id: readinessTimer
        interval: 250
        running: true
        repeat: true
        onTriggered: {
            var ready = root.backend !== null && typeof logos !== "undefined"
                && logos.isViewModuleReady("lez_faucet_ui")
            if (ready && !root.backendReady) {
                root.backendReady = true
                stop()
                root.beginBootstrap()
            }
        }
    }

    ScrollView {
        anchors.fill: parent
        contentWidth: availableWidth

        ColumnLayout {
            width: Math.min(680, root.width - Theme.spacing.xlarge * 2)
            anchors.horizontalCenter: parent.horizontalCenter
            spacing: Theme.spacing.large

            Item { Layout.preferredHeight: Theme.spacing.xlarge }

            LogosText {
                text: qsTr("LEZ Faucet")
                font.pixelSize: Theme.typography.titleText
                font.weight: Theme.typography.weightBold
                color: Theme.palette.text
                Layout.fillWidth: true
            }

            LogosText {
                visible: root.screenState === "welcome"
                text: qsTr("Create one public LEZ testnet account and fund it from the Pinata faucet. Testnet LEZ has no real-world value.")
                color: Theme.palette.textSecondary
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            ColumnLayout {
                visible: root.screenState === "welcome"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium

                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: warningColumn.implicitHeight + Theme.spacing.large * 2
                    radius: Theme.spacing.radiusLarge
                    color: Theme.palette.backgroundSecondary
                    border.color: Theme.palette.warning

                    ColumnLayout {
                        id: warningColumn
                        anchors.fill: parent
                        anchors.margins: Theme.spacing.large
                        LogosText {
                            text: qsTr("Important: this testnet wallet is not encrypted")
                            font.weight: Theme.typography.weightBold
                            color: Theme.palette.text
                            Layout.fillWidth: true
                        }
                        LogosText {
                            text: qsTr("LEZ v0.2.0 stores key material in a plaintext file on this Mac. The password below is required by the wallet API, but it does not encrypt that file. Do not reuse a real password.")
                            color: Theme.palette.textSecondary
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                        }
                    }
                }

                LogosTextField {
                    id: passwordField
                    Layout.fillWidth: true
                    placeholderText: qsTr("Wallet password (not encryption)")
                    echoMode: TextInput.Password
                }
                LogosTextField {
                    id: confirmField
                    Layout.fillWidth: true
                    placeholderText: qsTr("Confirm password")
                    echoMode: TextInput.Password
                }
                CheckBox {
                    id: plaintextAcknowledged
                    text: qsTr("I understand that this wallet file contains plaintext key material.")
                    Layout.fillWidth: true
                }
                LogosText {
                    visible: confirmField.text !== "" && passwordField.text !== confirmField.text
                    text: qsTr("Passwords do not match.")
                    color: Theme.palette.error
                }
                RowLayout {
                    Layout.fillWidth: true
                    LogosButton {
                        text: qsTr("Recover an account")
                        onClicked: root.screenState = "recovery_unavailable"
                    }
                    Item { Layout.fillWidth: true }
                    LogosButton {
                        text: qsTr("Create wallet")
                        enabled: passwordField.text !== ""
                            && passwordField.text === confirmField.text
                            && plaintextAcknowledged.checked
                        onClicked: {
                            var transientPassword = passwordField.text
                            passwordField.text = ""
                            confirmField.text = ""
                            root.submitCreate(transientPassword)
                            transientPassword = ""
                        }
                    }
                }
            }

            ColumnLayout {
                visible: ["booting", "opening", "compatibility", "creating", "initializing"].indexOf(root.screenState) >= 0
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                BusyIndicator { running: parent.visible; Layout.alignment: Qt.AlignHCenter }
                LogosText {
                    text: root.statusText
                    color: Theme.palette.text
                    horizontalAlignment: Text.AlignHCenter
                    Layout.fillWidth: true
                }
            }

            ColumnLayout {
                visible: root.screenState === "mnemonic"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Save your recovery phrase")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("Save it somewhere private before continuing. LEZ Faucet will forget the phrase as soon as you confirm that it is saved.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                TextArea {
                    Layout.fillWidth: true
                    readOnly: true
                    selectByMouse: true
                    wrapMode: TextEdit.Wrap
                    text: root.mnemonicText
                    color: Theme.palette.text
                    background: Rectangle {
                        color: Theme.palette.backgroundSecondary
                        radius: Theme.spacing.radiusLarge
                        border.color: Theme.palette.backgroundElevated
                    }
                }
                CheckBox {
                    id: mnemonicAcknowledged
                    text: qsTr("I saved my recovery phrase.")
                }
                LogosButton {
                    text: qsTr("Continue and initialize account")
                    enabled: mnemonicAcknowledged.checked
                    Layout.alignment: Qt.AlignRight
                    onClicked: root.acknowledgeMnemonic()
                }
            }

            ColumnLayout {
                visible: root.screenState === "initialization_required"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Initialize your public account")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("The account must be initialized on the LEZ testnet before it can receive a faucet claim.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton {
                    text: qsTr("Initialize account")
                    Layout.alignment: Qt.AlignRight
                    onClicked: root.initializeAccount()
                }
            }

            ColumnLayout {
                visible: root.screenState === "ready"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: root.statusText || qsTr("Your testnet account is ready")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("Balance")
                    color: Theme.palette.textSecondary
                }
                LogosText {
                    text: root.balanceText + qsTr(" LEZ")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: root.accountId
                    color: Theme.palette.textSecondary
                    elide: Text.ElideMiddle
                    Layout.fillWidth: true
                }
                RowLayout {
                    Layout.fillWidth: true
                    LogosButton {
                        text: qsTr("Refresh balance")
                        onClicked: root.refreshBalance()
                    }
                    Item { Layout.fillWidth: true }
                    LogosButton {
                        text: qsTr("Claim 150 LEZ")
                        onClicked: root.startClaimOnce()
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    LogosTextField {
                        id: targetField
                        Layout.fillWidth: true
                        placeholderText: qsTr("Target balance")
                        text: root.targetText
                        textInput.inputMethodHints: Qt.ImhDigitsOnly
                        textInput.maximumLength: 39
                        onTextChanged: root.targetText = text
                    }
                    LogosButton {
                        text: {
                            if (!FaucetFlow.isU128Decimal(root.targetText, false))
                                return qsTr("Enter a valid target")
                            return FaucetFlow.targetPending(root.balanceText, root.targetText)
                                ? qsTr("Claim to at least %1 LEZ (max 100 claims)").arg(root.targetText)
                                : qsTr("Target reached")
                        }
                        enabled: FaucetFlow.targetPending(root.balanceText, root.targetText)
                        onClicked: root.startClaimToTarget()
                    }
                }
                LogosText {
                    text: qsTr("Each claim adds 150 LEZ. Your final balance may be up to 149 LEZ above the target. One run is limited to 100 claims.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
            }

            ColumnLayout {
                visible: root.screenState === "claiming_once"
                    || root.screenState === "claiming_target"
                    || root.screenState === "cancelling"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                BusyIndicator { running: parent.visible; Layout.alignment: Qt.AlignHCenter }
                LogosText {
                    text: root.statusText
                    color: Theme.palette.text
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosText {
                    visible: root.screenState !== "claiming_once"
                    text: qsTr("%1 of %2 claims confirmed · Current balance: %3 LEZ")
                        .arg(root.completedClaims).arg(root.requiredClaims).arg(root.balanceText)
                    color: Theme.palette.textSecondary
                    horizontalAlignment: Text.AlignHCenter
                    Layout.fillWidth: true
                }
                LogosButton {
                    visible: root.screenState === "claiming_target"
                    text: qsTr("Stop after this claim")
                    Layout.alignment: Qt.AlignHCenter
                    onClicked: root.requestStop()
                }
            }

            ColumnLayout {
                visible: root.screenState === "version_mismatch"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Testnet version mismatch")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.error
                }
                LogosText {
                    text: qsTr("This faucet was built for LEZ v0.2.0, but the sequencer exposes different program IDs. Account initialization and claims are disabled to prevent transactions from being silently dropped.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton {
                    text: qsTr("Retry compatibility check")
                    onClicked: root.verifyCompatibility(root.hasAccount ? "ready" : "initialization_required")
                }
            }

            ColumnLayout {
                visible: root.screenState === "stale"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("The faucet is busy")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("The Pinata challenge changed too often. No claim was confirmed for this attempt. Refresh the balance and try again in a moment.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton { text: qsTr("Refresh balance"); onClicked: root.refreshBalance() }
            }

            ColumnLayout {
                visible: root.screenState === "outcome_unknown"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Claim outcome needs reconciliation")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.warning
                }
                LogosText {
                    text: qsTr("The claim was submitted, but the faucet could not prove whether it was included. Do not submit another claim yet: reconcile the balance first to avoid a duplicate attempt.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton {
                    text: qsTr("Reconcile balance")
                    onClicked: root.verifyCompatibility("ready")
                }
            }

            ColumnLayout {
                visible: root.screenState === "offline"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Can’t reach the LEZ testnet")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("The operation could not be confirmed. The displayed balance may be out of date; reconnect and refresh it before submitting another claim.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton { text: qsTr("Retry"); onClicked: root.retryFromOffline() }
            }

            ColumnLayout {
                visible: root.screenState === "recovery_unavailable"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Recovery is not available in this build")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.text
                }
                LogosText {
                    text: qsTr("The v0.1 faucet core cannot restore from a recovery phrase. Use the LEZ v0.2.0 wallet CLI to recover, then return to this faucet after recovery support is added.")
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton { text: qsTr("Back"); onClicked: root.screenState = "welcome" }
            }

            ColumnLayout {
                visible: root.screenState === "open_error" || root.screenState === "error"
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: root.screenState === "open_error"
                        ? qsTr("We couldn’t open this faucet wallet") : qsTr("Something went wrong")
                    font.pixelSize: Theme.typography.titleText
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.error
                }
                LogosText {
                    text: root.errorText
                    color: Theme.palette.textSecondary
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                RowLayout {
                    LogosButton {
                        text: qsTr("Recovery options")
                        onClicked: root.screenState = "recovery_unavailable"
                    }
                    LogosButton {
                        text: qsTr("Retry")
                        onClicked: root.hasAccount ? root.refreshBalance() : root.beginBootstrap()
                    }
                }
            }

            Item { Layout.preferredHeight: Theme.spacing.xlarge }
        }
    }
}
