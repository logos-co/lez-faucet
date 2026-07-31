// SPDX-License-Identifier: MIT OR Apache-2.0
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Logos.Theme
import Logos.Controls
import "FaucetFlow.js" as FaucetFlow

// One screen, one address, one button, one credit.
//
// The faucet owns no account and holds no credential. It reads public chain
// state, sends one unsigned transaction, and then either proves the recipient
// gained exactly the prize or refuses to say it did.
Rectangle {
    id: root
    color: Theme.palette.background

    readonly property var backend: typeof logos !== "undefined" && logos.module
        ? logos.module("lez_faucet_ui") : null
    property bool backendReady: false
    property bool bootstrapped: false
    property string bootstrapError: ""

    // -- pool ---------------------------------------------------------------
    property var pool: FaucetFlow.poolView(null)
    property string poolJobId: ""
    property bool poolPollInFlight: false
    property string poolError: ""

    // -- the address the user typed -----------------------------------------
    property string addressInput: ""
    // The canonical address the core returned for this exact input. Only this
    // is ever displayed; the raw input is never echoed back as if it were an
    // account the chain agreed exists.
    property var inspection: null
    property string inspectionForInput: ""
    property string inspectJobId: ""
    property bool inspectPollInFlight: false
    property string inspectionError: ""

    // -- the credit request -------------------------------------------------
    //
    // `creditAttempt` carries the request key minted for the current button
    // press. It is created in exactly one place and never re-created for a
    // re-send. See FaucetFlow.beginCreditAttempt for why.
    property var creditAttempt: null
    property string creditJobId: ""
    property string creditStage: "" // "" | "sending" | "running" | "interrupted"
    property bool creditPollInFlight: false
    // True only while the core is still before the point of no return. Once a
    // transaction has been sent there is nothing left to cancel, only to
    // reconcile, and no button may suggest otherwise.
    property bool cancellable: false
    property bool cancelRequested: false
    // The raw phase from the job envelope. It is compared, never displayed:
    // every sentence the user reads comes from FaucetFlow.phaseSentence.
    property string creditPhase: ""
    property string statusText: ""

    // -- what the result panel shows ----------------------------------------
    property string panel: "none" // "none" | "receipt" | "error" | "unknown"
    property var receipt: null
    property var failure: null
    property var unknownOutcome: null

    readonly property bool addressUsable: FaucetFlow.isPlausibleAddressInput(addressInput)
    readonly property bool creditLive: creditStage !== ""
    readonly property bool poolBlocked: pool.known && !pool.canClaim
    readonly property bool canRequest: bootstrapped && !creditLive && addressUsable && !poolBlocked

    // -- the three lamps -----------------------------------------------------
    //
    // The rail is drawn whether or not a claim is running, because its whole
    // point is to say what the five-minute step is *before* the button is
    // pressed. -1 means nothing is lit: either no claim is live, or the core
    // reported a phase this build does not recognise.
    readonly property var stages: [
        { title: qsTr("Solve"), detail: qsTr("Proof-of-work, usually seconds") },
        { title: qsTr("Submit"), detail: qsTr("One unsigned claim, no key needed") },
        { title: qsTr("Confirm"), detail: qsTr("Up to five minutes. Keep the app open.") }
    ]
    // The decision lives in FaucetFlow.railStage so that "a proven credit leaves
    // every lamp lit" is a tested rule rather than an accident of this binding.
    readonly property int currentStage: FaucetFlow.railStage(
        panel, creditLive, creditPhase, stages.length)

    // Addresses, hashes and shell commands are read character by character —
    // an l against a 1, a 0 against an O — so they are set in a fixed-pitch
    // face and aligned in columns.
    //
    // "monospace" is a CSS generic, not a font family, and Qt does not treat it
    // as one: on macOS it matches nothing and silently falls back to the
    // proportional default, which is how every hash in this view came to be set
    // in Public Sans. Name families that actually exist on each platform.
    readonly property string monoFamily: Qt.platform.os === "osx"
        ? "Menlo"
        : (Qt.platform.os === "windows" ? "Consolas" : "DejaVu Sans Mono")

    // ---- bridge helpers ----------------------------------------------------

    function watch(reply, onSuccess, onError) {
        if (typeof logos === "undefined" || !logos.watch) {
            onError("The Logos bridge is not available")
            return
        }
        logos.watch(reply,
            function(value) { onSuccess(value) },
            function(error) { onError(String(error || "Unknown backend error")) })
    }

    // Parse a bridge reply into { ok, ... }. A failure always carries a
    // structured error object so the view has one contract to dispatch on.
    function parseEnvelope(raw) {
        try {
            var object = JSON.parse(String(raw || "{}"))
            if (object.ok !== true) {
                var error = object.error
                if (!error || typeof error !== "object")
                    error = { code: "", message: String(error || "The request failed.") }
                return { ok: false, error: error }
            }
            return object
        } catch (parseFailure) {
            return { ok: false, error: { code: "", message: "The faucet returned an unreadable response." } }
        }
    }

    function resultOf(envelope) {
        return envelope && envelope.result !== undefined ? envelope.result : envelope
    }

    // ---- bootstrap ---------------------------------------------------------

    function beginBootstrap() {
        if (!backendReady || !backend)
            return
        bootstrapError = ""
        watch(backend.bootstrap(), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                bootstrapError = FaucetFlow.errorPresentation(envelope.error).title
                return
            }
            bootstrapped = true
            refreshPool()
            // A reloaded view rejoins a request that is already running. It
            // resumes by polling the existing job, and recovers the original
            // request key from that job's envelope, so no second key and no
            // second transaction can come out of a reload.
            var resumedJobId = String(envelope.active_job_id || "")
            if (resumedJobId !== "") {
                creditJobId = resumedJobId
                creditStage = "running"
                statusText = qsTr("Rejoining a request that is already running…")
            }
        }, function(error) {
            bootstrapError = String(error)
        })
    }

    // ---- pool status -------------------------------------------------------

    function refreshPool() {
        if (!backend || poolJobId !== "")
            return
        poolError = ""
        watch(backend.startFaucetInfo(), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                poolError = FaucetFlow.errorPresentation(envelope.error).title
                return
            }
            var jobId = String(envelope.job_id || "")
            if (jobId === "") {
                poolError = qsTr("The faucet did not report its pool status.")
                return
            }
            poolJobId = jobId
        }, function(error) {
            poolError = String(error)
        })
    }

    function pollPool() {
        if (poolJobId === "" || poolPollInFlight)
            return
        poolPollInFlight = true
        var jobId = poolJobId
        watch(backend.jobStatus(jobId), function(raw) {
            poolPollInFlight = false
            if (jobId !== poolJobId)
                return
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                poolError = FaucetFlow.errorPresentation(envelope.error).title
                poolJobId = ""
                return
            }
            var outcome = FaucetFlow.jobOutcome(envelope)
            if (!FaucetFlow.isTerminalOutcome(outcome))
                return
            if (outcome === "succeeded")
                pool = FaucetFlow.poolView(resultOf(envelope))
            else
                poolError = FaucetFlow.errorPresentation(envelope.error).title
            acknowledge(jobId)
            poolJobId = ""
        }, function(error) {
            poolPollInFlight = false
            poolError = String(error)
        })
    }

    // ---- recipient inspection ---------------------------------------------

    function clearInspection() {
        inspection = null
        inspectionForInput = ""
        inspectionError = ""
        inspectJobId = ""
    }

    // Look the address up so the user learns it is uninitialized before
    // spending a minute of proof-of-work on it. This is advisory only: the
    // core re-inspects inside the request and is the authority.
    function inspectAddress() {
        if (!backend || !addressUsable || inspectJobId !== "")
            return
        var inspected = addressInput
        inspectionError = ""
        watch(backend.startInspectRecipient(inspected), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                inspectionError = FaucetFlow.errorPresentation(envelope.error).title
                return
            }
            var jobId = String(envelope.job_id || "")
            if (jobId === "")
                return
            inspectJobId = jobId
            inspectionForInput = inspected
        }, function(error) {
            inspectionError = String(error)
        })
    }

    function pollInspection() {
        if (inspectJobId === "" || inspectPollInFlight)
            return
        inspectPollInFlight = true
        var jobId = inspectJobId
        watch(backend.jobStatus(jobId), function(raw) {
            inspectPollInFlight = false
            if (jobId !== inspectJobId)
                return
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                inspectionError = FaucetFlow.errorPresentation(envelope.error).title
                inspectJobId = ""
                return
            }
            var outcome = FaucetFlow.jobOutcome(envelope)
            if (!FaucetFlow.isTerminalOutcome(outcome))
                return
            if (outcome === "succeeded")
                inspection = FaucetFlow.inspectionView(resultOf(envelope))
            else
                inspectionError = FaucetFlow.errorPresentation(envelope.error).title
            acknowledge(jobId)
            inspectJobId = ""
        }, function(error) {
            inspectPollInFlight = false
            inspectionError = String(error)
        })
    }

    // ---- the credit request ------------------------------------------------

    // The one and only place a request key is minted.
    //
    // One press of the button is one key, and that key is stored for as long as
    // the attempt lives. Every re-send below goes through sendCreditRequest()
    // with the stored key, never through here.
    function requestCredit() {
        if (!canRequest || !backend)
            return
        panel = "none"
        receipt = null
        failure = null
        unknownOutcome = null
        cancelRequested = false
        creditPhase = ""
        creditAttempt = FaucetFlow.beginCreditAttempt(addressInput, FaucetFlow.newRequestKey)
        sendCreditRequest()
    }

    // Send — or re-send — the current attempt.
    //
    // Re-sending carries the *same* request key and the same address, and that
    // is the whole point. The core keys idempotency on that pair: a repeat
    // returns the original job rather than starting a second one. Minting a
    // fresh key here instead would turn a lost reply into a second on-chain
    // claim, because a lost reply is not evidence that nothing happened — the
    // request may already be solving, submitting or reconciling.
    function sendCreditRequest() {
        if (!backend || !creditAttempt)
            return
        creditStage = "sending"
        statusText = qsTr("Sending your request to the faucet…")
        watch(backend.startRequestDrop(creditAttempt.address, creditAttempt.requestKey),
            function(raw) {
                var envelope = parseEnvelope(raw)
                if (!envelope.ok) {
                    var presented = FaucetFlow.errorPresentation(envelope.error)
                    // The core already has a request under this key. Adopt its
                    // job instead of starting anything: this is the shape a
                    // successful re-send can take.
                    if (presented.code === "drop_in_progress"
                            && String(backend.activeJobId || "") !== "") {
                        creditJobId = String(backend.activeJobId)
                        creditStage = "running"
                        return
                    }
                    presentFailure(envelope.error)
                    return
                }
                var jobId = String(envelope.job_id || "")
                if (jobId === "") {
                    // No identifier came back, so this view cannot follow the
                    // request — but the request may well be running. Re-send
                    // the same key rather than assume nothing happened.
                    interruptCreditRequest(qsTr("The faucet did not return a request identifier."))
                    return
                }
                creditJobId = jobId
                creditStage = "running"
            },
            function(error) {
                interruptCreditRequest(String(error))
            })
    }

    // Lost contact before a job identifier was known. Keep the attempt — and
    // its key — and retry the identical call until it lands.
    function interruptCreditRequest(reason) {
        creditStage = "interrupted"
        statusText = String(reason || "")
    }

    function pollCredit() {
        if (creditJobId === "" || creditPollInFlight)
            return
        creditPollInFlight = true
        var jobId = creditJobId
        watch(backend.jobStatus(jobId), function(raw) {
            creditPollInFlight = false
            if (jobId !== creditJobId)
                return
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                // The job is still the core's; keep polling rather than
                // deciding anything from a failed read.
                statusText = qsTr("Connection interrupted. Reconnecting to your request…")
                return
            }

            if (!creditAttempt) {
                // Resumed after a reload: take the original key back from the
                // envelope instead of inventing a new one.
                var rejoined = FaucetFlow.rejoinCreditAttempt(envelope)
                if (rejoined)
                    creditAttempt = rejoined
            }

            var outcome = FaucetFlow.jobOutcome(envelope)
            if (!FaucetFlow.isTerminalOutcome(outcome)) {
                // A phase name is a state-machine label, never a sentence.
                statusText = FaucetFlow.phaseSentence(envelope.phase)
                creditPhase = String(envelope.phase || "")
                cancelRequested = envelope.cancel_requested === true
                cancellable = FaucetFlow.cancelAvailable(
                    envelope.status, envelope.phase, envelope.cancel_requested)
                return
            }

            cancellable = false
            var terminalEnvelope = envelope
            acknowledgeThen(jobId, function() {
                finishCredit(outcome, terminalEnvelope)
            })
        }, function(error) {
            creditPollInFlight = false
            statusText = qsTr("Connection interrupted. Reconnecting to your request…")
        })
    }

    function finishCredit(outcome, envelope) {
        creditJobId = ""
        creditStage = ""
        creditPollInFlight = false
        cancelRequested = false
        creditPhase = ""

        if (outcome === "succeeded") {
            var proven = FaucetFlow.receiptView(resultOf(envelope))
            if (proven) {
                receipt = proven
                panel = "receipt"
                statusText = ""
                creditAttempt = null
                refreshPool()
                inspectAddress()
                return
            }
            // The core called it a success but the payload does not prove one.
            // Never invent a receipt: say the outcome is unproven instead.
            presentUnknown({ code: "outcome_unknown", message:
                qsTr("The faucet reported success without proof that the account was credited."),
                details: {} })
            return
        }
        if (outcome === "outcome_unknown") {
            presentUnknown(envelope.error)
            return
        }
        if (outcome === "cancelled") {
            statusText = qsTr("Stopped. No transaction was sent.")
            creditAttempt = null
            return
        }
        presentFailure(envelope.error)
    }

    function presentFailure(error) {
        creditJobId = ""
        creditStage = ""
        cancellable = false
        creditPhase = ""
        statusText = ""
        var presented = FaucetFlow.errorPresentation(error)
        if (presented.code === "outcome_unknown") {
            presentUnknown(error)
            return
        }
        failure = presented
        panel = "error"
        // The attempt is over. A further request is a new press of the button,
        // which mints its own key; this one is spent and must not be reused.
        creditAttempt = null
    }

    function presentUnknown(error) {
        creditJobId = ""
        creditStage = ""
        cancellable = false
        creditPhase = ""
        statusText = ""
        creditAttempt = null
        var pinned = inspection && inspectionForInput === addressInput ? inspection.address : ""
        unknownOutcome = FaucetFlow.unknownOutcomeView(error, pinned)
        panel = "unknown"
    }

    function cancelCreditRequest() {
        if (!backend || creditJobId === "")
            return
        cancelRequested = true
        statusText = qsTr("Stopping. Once a claim has been sent it cannot be taken back.")
        watch(backend.cancelJob(creditJobId), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok)
                statusText = FaucetFlow.errorPresentation(envelope.error).title
        }, function(error) {
            statusText = String(error)
        })
    }

    // ---- acknowledgement ---------------------------------------------------

    function acknowledge(jobId) {
        if (!backend || jobId === "")
            return
        watch(backend.acknowledgeJob(jobId), function(raw) {}, function(error) {})
    }

    // A terminal envelope is replayed by the backend until it is acknowledged,
    // so a failed acknowledgement must not clear the local view either: keep
    // polling and present the result once it sticks.
    function acknowledgeThen(jobId, onAcknowledged) {
        watch(backend.acknowledgeJob(jobId), function(raw) {
            var envelope = parseEnvelope(raw)
            if (!envelope.ok) {
                statusText = qsTr("Could not confirm the result yet. Reconnecting…")
                return
            }
            onAcknowledged()
        }, function(error) {
            statusText = qsTr("Could not confirm the result yet. Reconnecting…")
        })
    }

    function copyInitializationCommand(command) {
        if (!backend || !command)
            return
        watch(backend.copyText(command), function(raw) {
            if (parseEnvelope(raw).ok)
                inspectionError = ""
        }, function(error) {})
    }

    // ---- timers ------------------------------------------------------------

    Timer {
        id: pollTimer
        interval: 500
        repeat: true
        running: root.poolJobId !== "" || root.inspectJobId !== "" || root.creditJobId !== ""
        onTriggered: {
            root.pollPool()
            root.pollInspection()
            root.pollCredit()
        }
    }

    // Re-sends the interrupted attempt with its stored key. Safe to repeat
    // precisely because the key never changes.
    Timer {
        id: reconnectTimer
        interval: 3000
        repeat: true
        running: root.creditStage === "interrupted"
        onTriggered: root.sendCreditRequest()
    }

    // Waits until the user has stopped typing before looking the account up.
    Timer {
        id: inspectDebounce
        interval: 800
        repeat: false
        onTriggered: root.inspectAddress()
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

    // ---- layout ------------------------------------------------------------

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
                textFormat: Text.PlainText
                font.pixelSize: Theme.typography.titleText
                font.weight: Theme.typography.weightBold
                color: Theme.palette.text
                Layout.fillWidth: true
            }

            LogosText {
                text: qsTr("Enter a public LEZ testnet account and request 150 LEZ for it. Testnet LEZ has no real-world value.")
                textFormat: Text.PlainText
                color: Theme.palette.textSecondary
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            // -- 1. pool status ---------------------------------------------
            //
            // A readout, not a headline. The balance is the one figure worth
            // setting large; everything qualifying it sits underneath it in
            // descending weight. There is deliberately no proportional meter:
            // the core reports the pool's *current* balance and nothing else,
            // so any "x% full" bar would need a capacity this app has never
            // been told and could not keep true across a refill.
            Rectangle {
                Layout.fillWidth: true
                implicitHeight: poolColumn.implicitHeight + Theme.spacing.large * 2
                radius: Theme.spacing.radiusLarge
                color: Theme.palette.backgroundTertiary
                border.color: root.poolBlocked ? Theme.palette.warning
                                               : Theme.palette.borderSecondary

                ColumnLayout {
                    id: poolColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacing.large
                    spacing: Theme.spacing.small

                    RowLayout {
                        Layout.fillWidth: true
                        LogosText {
                            text: qsTr("Faucet pool")
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.secondaryText
                            font.weight: Theme.typography.weightMedium
                            color: Theme.palette.textTertiary
                        }
                        Item { Layout.fillWidth: true }
                        LogosText {
                            visible: root.poolJobId !== ""
                            text: qsTr("Reading…")
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.secondaryText
                            color: Theme.palette.textTertiary
                        }
                        // Sized down from the stock 200x50: re-reading the pool
                        // is a secondary action and must not read as loudly as
                        // the one button on the screen that spends a claim.
                        LogosButton {
                            text: qsTr("Refresh")
                            enabled: root.bootstrapped && root.poolJobId === ""
                            implicitWidth: 88
                            implicitHeight: 32
                            onClicked: root.refreshPool()
                        }
                    }

                    // The readout itself.
                    RowLayout {
                        visible: root.pool.known
                        Layout.fillWidth: true
                        spacing: Theme.spacing.small
                        LogosText {
                            text: FaucetFlow.groupDigits(root.pool.poolBalance)
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.panelTitleText
                            font.weight: Theme.typography.weightBold
                            color: Theme.palette.text
                        }
                        LogosText {
                            text: qsTr("LEZ")
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.primaryText
                            color: Theme.palette.textSecondary
                        }
                        Item { Layout.fillWidth: true }
                    }

                    RowLayout {
                        visible: root.pool.known
                        Layout.fillWidth: true
                        spacing: Theme.spacing.small
                        LogosText {
                            text: qsTr("~%1 claims left at this instant")
                                .arg(FaucetFlow.groupDigits(root.pool.claimsRemaining))
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.secondaryText
                            color: Theme.palette.textSecondary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                        // Prize and difficulty as badges: two fixed facts about
                        // the protocol, not prose to be read twice.
                        Rectangle {
                            implicitWidth: prizeBadge.implicitWidth + Theme.spacing.medium
                            implicitHeight: prizeBadge.implicitHeight + Theme.spacing.small
                            radius: Theme.spacing.radiusSmall
                            color: "transparent"
                            border.color: Theme.palette.primary
                            border.width: 1
                            LogosText {
                                id: prizeBadge
                                anchors.centerIn: parent
                                text: qsTr("%1 LEZ / claim").arg(
                                    FaucetFlow.groupDigits(root.pool.prizeAmount))
                                textFormat: Text.PlainText
                                font.pixelSize: Theme.typography.secondaryText
                                font.weight: Theme.typography.weightMedium
                                color: Theme.palette.primary
                            }
                        }
                        Rectangle {
                            visible: root.pool.difficultyBits !== ""
                            implicitWidth: powBadge.implicitWidth + Theme.spacing.medium
                            implicitHeight: powBadge.implicitHeight + Theme.spacing.small
                            radius: Theme.spacing.radiusSmall
                            color: "transparent"
                            border.color: Theme.palette.border
                            border.width: 1
                            LogosText {
                                id: powBadge
                                anchors.centerIn: parent
                                text: qsTr("%1-bit PoW").arg(root.pool.difficultyBits)
                                textFormat: Text.PlainText
                                font.pixelSize: Theme.typography.secondaryText
                                font.weight: Theme.typography.weightMedium
                                color: Theme.palette.textSecondary
                            }
                        }
                    }

                    LogosText {
                        visible: root.pool.known
                        text: qsTr("The pool is a finite, shared resource: everyone on the testnet claims from the same balance, so this figure moves without you.")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.textTertiary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.pool.blockedSentence !== ""
                        text: root.pool.blockedSentence
                        textFormat: Text.PlainText
                        color: Theme.palette.warning
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: !root.pool.known && root.poolError === ""
                        text: root.bootstrapped ? qsTr("Reading the pool…")
                                                : qsTr("Connecting to the LEZ testnet…")
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.poolError !== ""
                        text: root.poolError
                        textFormat: Text.PlainText
                        color: Theme.palette.error
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.bootstrapError !== ""
                        text: root.bootstrapError
                        textFormat: Text.PlainText
                        color: Theme.palette.error
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                }
            }

            // -- 2. the address ---------------------------------------------
            LogosText {
                text: qsTr("Public LEZ address")
                textFormat: Text.PlainText
                font.pixelSize: Theme.typography.secondaryText
                font.weight: Theme.typography.weightMedium
                color: Theme.palette.textTertiary
                Layout.fillWidth: true
                Layout.bottomMargin: -Theme.spacing.small
            }

            LogosTextField {
                id: addressField
                Layout.fillWidth: true
                enabled: !root.creditLive
                placeholderText: qsTr("Public/<account ID> or bare account ID")
                // "Public/" plus a 32-byte base58 id, and not one character more.
                textInput.maximumLength: 71
                onTextChanged: {
                    root.addressInput = text
                    // Changing the address invalidates everything the previous
                    // one established, including any result still on screen.
                    root.clearInspection()
                    root.panel = "none"
                    root.receipt = null
                    root.failure = null
                    root.unknownOutcome = null
                    inspectDebounce.restart()
                }
            }

            LogosText {
                visible: root.addressInput !== "" && !root.addressUsable
                text: qsTr("Enter the bare Base58 account ID, or Public/<account ID>. Private accounts cannot receive a faucet claim.")
                textFormat: Text.PlainText
                color: Theme.palette.textSecondary
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            // The account the core resolved. Displayed instead of the raw
            // input, which has no standing until the chain has agreed with it.
            ColumnLayout {
                visible: root.inspection !== null
                Layout.fillWidth: true
                spacing: Theme.spacing.tiny
                LogosText {
                    text: root.inspection ? root.inspection.address : ""
                    textFormat: Text.PlainText
                    font.family: root.monoFamily
                    font.pixelSize: Theme.typography.secondaryText
                    color: Theme.palette.textSecondary
                    elide: Text.ElideMiddle
                    Layout.fillWidth: true
                }
                LogosText {
                    text: root.inspection ? root.inspection.summary : ""
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.typography.secondaryText
                    // Eligibility is the one thing the user needs off this
                    // block, so it carries the only colour in it.
                    color: root.inspection && root.inspection.eligible
                        ? Theme.palette.success : Theme.palette.warning
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosText {
                    visible: root.inspection !== null && root.inspection.balance !== ""
                    text: qsTr("Current balance %1 LEZ").arg(
                        FaucetFlow.groupDigits(root.inspection ? root.inspection.balance : ""))
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.typography.secondaryText
                    color: Theme.palette.textSecondary
                    Layout.fillWidth: true
                }
                LogosText {
                    visible: root.inspection !== null
                        && root.inspection.initializationCommand !== ""
                    text: root.inspection ? root.inspection.initializationCommand : ""
                    textFormat: Text.PlainText
                    font.family: root.monoFamily
                    color: Theme.palette.text
                    wrapMode: Text.WrapAnywhere
                    Layout.fillWidth: true
                }
                LogosButton {
                    visible: root.inspection !== null
                        && root.inspection.initializationCommand !== ""
                    text: qsTr("Copy command")
                    onClicked: root.copyInitializationCommand(
                        root.inspection ? root.inspection.initializationCommand : "")
                }
            }

            LogosText {
                visible: root.inspectionError !== ""
                text: root.inspectionError
                textFormat: Text.PlainText
                color: Theme.palette.error
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            // -- 3. the button ----------------------------------------------
            //
            // The only saturated element on the screen, because it is the only
            // thing the user came here to do. LogosButton ships one grey
            // appearance for every button in the product, which left the
            // request indistinguishable from Refresh; background and
            // contentItem are overridden here rather than in the design system
            // so nothing else in Basecamp changes.
            //
            // The label is near-black on orange (about 6.6:1) rather than the
            // stock white (about 2.8:1, which fails AA on this fill).
            LogosButton {
                id: requestButton
                text: qsTr("Request 150 LEZ")
                enabled: root.canRequest
                Layout.fillWidth: true
                onClicked: root.requestCredit()
                background: Rectangle {
                    radius: Theme.spacing.radiusXlarge
                    color: !requestButton.enabled
                           ? Theme.palette.backgroundMuted
                           : (requestButton.isActive ? Theme.palette.primaryHover
                                                     : Theme.palette.primary)
                    border.width: requestButton.enabled ? 0 : 1
                    border.color: Theme.palette.border
                }
                contentItem: LogosText {
                    text: requestButton.text
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.typography.primaryText
                    font.weight: Theme.typography.weightBold
                    color: requestButton.enabled ? Theme.palette.background
                                                 : Theme.palette.textMuted
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }
            }

            // -- 3b. what pressing it costs ---------------------------------
            //
            // Drawn before the press, not after. The old view disclosed the
            // five-minute reconcile only once the user was already inside it.
            Rectangle {
                Layout.fillWidth: true
                implicitHeight: railColumn.implicitHeight + Theme.spacing.large * 2
                radius: Theme.spacing.radiusLarge
                color: Theme.palette.backgroundTertiary
                border.color: Theme.palette.borderSecondary

                ColumnLayout {
                    id: railColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacing.large
                    spacing: Theme.spacing.medium

                    LogosText {
                        text: root.creditLive && root.statusText !== ""
                              ? root.statusText
                              : qsTr("What happens when you press")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        font.weight: Theme.typography.weightMedium
                        color: root.creditLive ? Theme.palette.text
                                               : Theme.palette.textTertiary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacing.medium

                        Repeater {
                            model: root.stages
                            delegate: ColumnLayout {
                                id: stageItem
                                required property int index
                                required property var modelData
                                readonly property bool isDone: root.currentStage > index
                                readonly property bool isActive: root.currentStage === index
                                readonly property color lamp:
                                    isDone ? Theme.palette.success
                                           : (isActive ? Theme.palette.primary
                                                       : Theme.palette.textTertiary)
                                Layout.fillWidth: true
                                Layout.alignment: Qt.AlignTop
                                spacing: Theme.spacing.small

                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 0
                                    Rectangle {
                                        implicitWidth: 20
                                        implicitHeight: 20
                                        radius: 10
                                        color: "transparent"
                                        border.color: stageItem.lamp
                                        border.width: 1
                                        LogosText {
                                            anchors.centerIn: parent
                                            text: String(stageItem.index + 1)
                                            textFormat: Text.PlainText
                                            font.pixelSize: Theme.typography.secondaryText
                                            font.weight: Theme.typography.weightMedium
                                            color: stageItem.lamp
                                        }
                                    }
                                    Rectangle {
                                        visible: stageItem.index < root.stages.length - 1
                                        Layout.fillWidth: true
                                        implicitHeight: 1
                                        color: stageItem.isDone ? Theme.palette.success
                                                                : Theme.palette.borderSecondary
                                    }
                                }

                                LogosText {
                                    text: stageItem.modelData.title
                                    textFormat: Text.PlainText
                                    font.pixelSize: Theme.typography.secondaryText
                                    font.weight: Theme.typography.weightMedium
                                    color: stageItem.isDone || stageItem.isActive
                                           ? Theme.palette.text : Theme.palette.textSecondary
                                    Layout.fillWidth: true
                                }
                                LogosText {
                                    text: stageItem.modelData.detail
                                    textFormat: Text.PlainText
                                    font.pixelSize: Theme.typography.secondaryText
                                    // The slow step keeps its caution colour
                                    // until it is actually behind the user.
                                    color: stageItem.isDone
                                           ? Theme.palette.textTertiary
                                           : (stageItem.index === root.stages.length - 1
                                              ? Theme.palette.warning
                                              : Theme.palette.textTertiary)
                                    wrapMode: Text.WordWrap
                                    Layout.fillWidth: true
                                }
                            }
                        }
                    }
                }
            }

            // -- 4. progress, cancel, and the result ------------------------
            ColumnLayout {
                visible: root.creditLive
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                // The live sentence is drawn once, as the rail's heading. It is
                // deliberately not repeated here.
                BusyIndicator { running: parent.visible; Layout.alignment: Qt.AlignHCenter }
                LogosText {
                    // The one honestly slow stage. Its 300 s bound is stated in
                    // plain language at the moment it applies, along with the
                    // one thing quitting would cost: this app's in-session
                    // refusal to pay the same account twice.
                    visible: root.creditPhase === "reconciling"
                    text: qsTr("This step can take up to five minutes. Keep the app open: if you quit before it finishes, the claim stays unconfirmed and this app cannot stop a second request from crediting the same account twice.")
                    textFormat: Text.PlainText
                    color: Theme.palette.textSecondary
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosText {
                    visible: root.creditStage === "interrupted"
                    text: qsTr("Reconnecting to the same request. Nothing new is being sent.")
                    textFormat: Text.PlainText
                    color: Theme.palette.textSecondary
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
                LogosButton {
                    visible: root.creditStage === "interrupted"
                    text: qsTr("Reconnect now")
                    Layout.alignment: Qt.AlignHCenter
                    onClicked: root.sendCreditRequest()
                }
                LogosButton {
                    visible: root.cancellable
                    text: qsTr("Cancel")
                    Layout.alignment: Qt.AlignHCenter
                    onClicked: root.cancelCreditRequest()
                }
                LogosText {
                    visible: root.cancelRequested
                    text: qsTr("Stop requested. If the claim has already been sent it will still be reconciled.")
                    textFormat: Text.PlainText
                    color: Theme.palette.textSecondary
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }
            }

            LogosText {
                visible: !root.creditLive && root.statusText !== "" && root.panel === "none"
                text: root.statusText
                textFormat: Text.PlainText
                color: Theme.palette.textSecondary
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            // Receipt. Shown only for a proven credit.
            Rectangle {
                visible: root.panel === "receipt" && root.receipt !== null
                Layout.fillWidth: true
                implicitHeight: receiptColumn.implicitHeight + Theme.spacing.large * 2
                radius: Theme.spacing.radiusLarge
                color: Theme.palette.backgroundTertiary
                border.color: Theme.palette.success

                ColumnLayout {
                    id: receiptColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacing.large
                    spacing: Theme.spacing.small

                    // The delta is the fact. The two balances that prove it sit
                    // underneath, and the hashes that prove *those* sit under
                    // them, in the order someone checking the claim reads them.
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacing.small
                        LogosText {
                            text: qsTr("+%1 LEZ").arg(
                                FaucetFlow.groupDigits(root.receipt ? root.receipt.amount : ""))
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.typography.panelTitleText
                            font.weight: Theme.typography.weightBold
                            color: Theme.palette.success
                        }
                        Item { Layout.fillWidth: true }
                        Rectangle {
                            implicitWidth: confirmedBadge.implicitWidth + Theme.spacing.medium
                            implicitHeight: confirmedBadge.implicitHeight + Theme.spacing.small
                            radius: Theme.spacing.radiusSmall
                            color: "transparent"
                            border.color: Theme.palette.success
                            border.width: 1
                            LogosText {
                                id: confirmedBadge
                                anchors.centerIn: parent
                                text: qsTr("Confirmed on chain")
                                textFormat: Text.PlainText
                                font.pixelSize: Theme.typography.secondaryText
                                font.weight: Theme.typography.weightMedium
                                color: Theme.palette.success
                            }
                        }
                    }
                    LogosText {
                        text: qsTr("Balance %1 → %2 LEZ")
                            .arg(FaucetFlow.groupDigits(root.receipt ? root.receipt.balanceBefore : ""))
                            .arg(FaucetFlow.groupDigits(root.receipt ? root.receipt.balanceAfter : ""))
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.topMargin: Theme.spacing.small
                        implicitHeight: 1
                        color: Theme.palette.borderSubtle
                    }

                    LogosText {
                        text: qsTr("Account")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.textTertiary
                    }
                    LogosText {
                        text: root.receipt ? root.receipt.address : ""
                        textFormat: Text.PlainText
                        font.family: root.monoFamily
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.text
                        wrapMode: Text.WrapAnywhere
                        Layout.fillWidth: true
                    }
                    LogosText {
                        text: qsTr("Transaction")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.textTertiary
                        Layout.topMargin: Theme.spacing.tiny
                    }
                    LogosText {
                        text: root.receipt ? root.receipt.txHash : ""
                        textFormat: Text.PlainText
                        font.family: root.monoFamily
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.text
                        wrapMode: Text.WrapAnywhere
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.receipt !== null && root.receipt.retried
                        text: qsTr("Another claimant won the challenge first, so this took more than one attempt. Only one claim was credited.")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.secondaryText
                        color: Theme.palette.textTertiary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        Layout.topMargin: Theme.spacing.tiny
                    }
                }
            }

            // Unproven outcome. Deliberately offers no retry: a second request
            // while the first may still be pending is a second 150 LEZ.
            Rectangle {
                visible: root.panel === "unknown" && root.unknownOutcome !== null
                Layout.fillWidth: true
                implicitHeight: unknownColumn.implicitHeight + Theme.spacing.large * 2
                radius: Theme.spacing.radiusLarge
                color: Theme.palette.backgroundTertiary
                border.color: Theme.palette.warning

                ColumnLayout {
                    id: unknownColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacing.large
                    spacing: Theme.spacing.small
                    LogosText {
                        text: qsTr("This claim could not be confirmed")
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.panelTitleText
                        font.weight: Theme.typography.weightBold
                        color: Theme.palette.warning
                        Layout.fillWidth: true
                    }
                    LogosText {
                        text: qsTr("The claim was sent, but this app could not prove what became of it. Check the account's balance with your own tools before doing anything else. This app will not send another claim to that account.")
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.unknownOutcome !== null && root.unknownOutcome.address !== ""
                        text: qsTr("Account: %1").arg(
                            root.unknownOutcome ? root.unknownOutcome.address : "")
                        textFormat: Text.PlainText
                        color: Theme.palette.text
                        wrapMode: Text.WrapAnywhere
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.unknownOutcome !== null
                            && root.unknownOutcome.balanceBefore !== ""
                        text: qsTr("Balance before the claim: %1 LEZ").arg(
                            FaucetFlow.groupDigits(
                                root.unknownOutcome ? root.unknownOutcome.balanceBefore : ""))
                        textFormat: Text.PlainText
                        color: Theme.palette.text
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.unknownOutcome !== null && root.unknownOutcome.txHash !== ""
                        text: qsTr("Transaction: %1").arg(
                            root.unknownOutcome ? root.unknownOutcome.txHash : "")
                        textFormat: Text.PlainText
                        font.family: root.monoFamily
                        color: Theme.palette.text
                        wrapMode: Text.WrapAnywhere
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.unknownOutcome !== null && root.unknownOutcome.message !== ""
                        text: root.unknownOutcome ? root.unknownOutcome.message : ""
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                }
            }

            // Failure. The retry offer is decided by the error's code alone.
            Rectangle {
                visible: root.panel === "error" && root.failure !== null
                Layout.fillWidth: true
                implicitHeight: failureColumn.implicitHeight + Theme.spacing.large * 2
                radius: Theme.spacing.radiusLarge
                color: Theme.palette.backgroundTertiary
                border.color: Theme.palette.error

                ColumnLayout {
                    id: failureColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacing.large
                    spacing: Theme.spacing.small
                    LogosText {
                        text: root.failure ? root.failure.title : ""
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.typography.panelTitleText
                        font.weight: Theme.typography.weightBold
                        color: Theme.palette.error
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        text: root.failure ? root.failure.guidance : ""
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.failure !== null && root.failure.message !== ""
                        text: root.failure ? root.failure.message : ""
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                    LogosText {
                        visible: root.failure !== null
                            && root.failure.initializationCommand !== ""
                        text: root.failure ? root.failure.initializationCommand : ""
                        textFormat: Text.PlainText
                        font.family: root.monoFamily
                        color: Theme.palette.text
                        wrapMode: Text.WrapAnywhere
                        Layout.fillWidth: true
                    }
                    LogosButton {
                        visible: root.failure !== null
                            && root.failure.initializationCommand !== ""
                        text: qsTr("Copy command")
                        onClicked: root.copyInitializationCommand(
                            root.failure ? root.failure.initializationCommand : "")
                    }
                    LogosText {
                        visible: root.failure !== null && root.failure.newAttempt
                        text: qsTr("Nothing was credited. Requesting again starts a completely new request.")
                        textFormat: Text.PlainText
                        color: Theme.palette.textSecondary
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                    }
                }
            }

            // -- what this app cannot do for you ----------------------------
            //
            // Still said in full, still unhidden — but at the weight of a
            // footnote rather than of the receipt above it.
            Rectangle {
                Layout.fillWidth: true
                Layout.topMargin: Theme.spacing.small
                implicitHeight: 1
                color: Theme.palette.borderSubtle
            }

            LogosText {
                text: qsTr("This app stores nothing between runs. If you quit while a claim is running it cannot reconcile that claim afterwards — check the account's balance yourself before requesting again.")
                textFormat: Text.PlainText
                font.pixelSize: Theme.typography.secondaryText
                color: Theme.palette.textTertiary
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            LogosText {
                visible: root.pool.poolAddress !== ""
                text: qsTr("Pool account: %1").arg(root.pool.poolAddress)
                textFormat: Text.PlainText
                font.family: root.monoFamily
                font.pixelSize: Theme.typography.secondaryText
                color: Theme.palette.textMuted
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }

            Item { Layout.preferredHeight: Theme.spacing.xlarge }
        }
    }
}
