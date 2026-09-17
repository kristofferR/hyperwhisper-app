import Foundation
import CoreGraphics
import Testing
@testable import HyperWhisper

@MainActor
struct PushToTalkStreamingSessionTests {
    private func eventually(_ condition: () -> Bool) async {
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while !condition(), ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(1))
        }
        #expect(condition())
    }

    @Test func releaseDrainsOnceAndKeepsOwnershipUntilFinished() async {
        let session = PushToTalkStreamingSession()
        var stops: [Bool] = []
        var drained = false
        var ended = false
        let start = Task<Void, Never> {}
        await start.value
        session.begin(startTask: start, isRecording: { true }, stop: { cancelled in
            stops.append(cancelled)
            await eventually { drained }
        }, onEnd: { ended = true })

        #expect(session.isActive)
        #expect(stops.isEmpty)
        session.end(cancelled: false)
        session.end(cancelled: false)
        await eventually { stops.count == 1 }
        #expect(stops == [false])
        #expect(session.isActive)
        #expect(!ended)
        drained = true
        await eventually { ended }
        #expect(!session.isActive)
    }

    @Test func releaseDuringConnectionCancelsStartupBeforeStopping() async {
        let session = PushToTalkStreamingSession()
        var connecting = false
        var cleanedUp = false
        var ended = false
        var stopSawCleanup = false
        let start = Task<Void, Never> {
            connecting = true
            do { try await Task.sleep(for: .seconds(30)) } catch { }
            cleanedUp = Task.isCancelled
        }
        session.begin(startTask: start, isRecording: { false }, stop: { _ in
            stopSawCleanup = cleanedUp
        }, onEnd: { ended = true })
        await eventually { connecting }
        session.end(cancelled: false)
        await eventually { ended }
        #expect(cleanedUp)
        #expect(stopSawCleanup)
        #expect(!session.isActive)
    }

    @Test func cancellationOverridesReleaseWhileStartupFinishes() async {
        let session = PushToTalkStreamingSession()
        var stoppedAsCancelled = false
        var ended = false
        let start = Task<Void, Never> {}
        session.begin(startTask: start, isRecording: { true }, stop: { cancelled in
            stoppedAsCancelled = cancelled
        }, onEnd: { ended = true })
        session.end(cancelled: false)
        session.end(cancelled: true)
        await eventually { ended }
        #expect(stoppedAsCancelled)
    }

    @Test func failedStartupReleasesOwnershipAndAllowsAnotherHold() async {
        let session = PushToTalkStreamingSession()
        var ended = false
        session.begin(startTask: Task {}, isRecording: { false }, stop: { _ in }, onEnd: { ended = true })
        await eventually { ended }
        #expect(!session.isActive)

        ended = false
        let start = Task<Void, Never> {}
        await start.value
        session.begin(startTask: start, isRecording: { true }, stop: { _ in }, onEnd: { ended = true })
        #expect(session.isActive)
        session.end(cancelled: false)
        await eventually { ended }
        #expect(!session.isActive)
    }

    @Test func legacyShortcutBackupsDoNotOverrideStreamingPreference() throws {
        let legacy = Data(#"{"pushToTalkMode":"rightOption","pushToTalkDoublePressEnabled":false}"#.utf8)
        var settings = try JSONDecoder().decode(BackupShortcutSettings.self, from: legacy)
        #expect(settings.pushToTalkUsesStreaming == nil)
        settings.pushToTalkUsesStreaming = true
        let restored = try JSONDecoder().decode(BackupShortcutSettings.self, from: JSONEncoder().encode(settings))
        #expect(restored.pushToTalkUsesStreaming == true)
    }

    @Test func streamedKeyboardEventsCannotCancelTheHeldModifier() throws {
        let source = try #require(CGEventSource(stateID: .privateState))
        let event = try #require(CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true))
        #expect(BareModifierKeyMonitor.isAppGeneratedEvent(event))
        event.setIntegerValueField(.eventSourceUnixProcessID, value: 0)
        #expect(!BareModifierKeyMonitor.isAppGeneratedEvent(event))
    }
}
