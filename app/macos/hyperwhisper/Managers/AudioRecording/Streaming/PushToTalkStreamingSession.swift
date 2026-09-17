import Foundation

/// Owns a held streaming session through connection startup and final transcript drain.
@MainActor
final class PushToTalkStreamingSession {
    private var startTask: Task<Void, Never>?
    private var endTask: Task<Void, Never>?
    private var cancelRequested = false
    private var stop: ((Bool) async -> Void)?
    private var onEnd: (() -> Void)?
    private var sessionID: UUID?
    private var isStarting = false

    var isActive: Bool { startTask != nil }

    func begin(
        startTask: Task<Void, Never>,
        isRecording: @escaping () -> Bool,
        stop: @escaping (Bool) async -> Void,
        onEnd: @escaping () -> Void
    ) {
        precondition(!isActive)
        let id = UUID()
        sessionID = id
        self.startTask = startTask
        self.stop = stop
        self.onEnd = onEnd
        cancelRequested = false
        isStarting = true
        Task {
            await startTask.value
            guard sessionID == id else { return }
            isStarting = false
            if !isRecording() { end(cancelled: true) }
        }
    }

    func recordingBecameIdle(isStreamingActive: Bool) {
        // Global idle events can come from an older batch or file transcription.
        // Startup completion handles its own failure; only end an established,
        // owned stream once the streaming flow has actually stopped.
        if !isStarting && !isStreamingActive { end(cancelled: true) }
    }

    func end(cancelled: Bool) {
        guard let startTask else { return }
        cancelRequested = cancelRequested || cancelled
        guard endTask == nil else { return }
        // A release during permission checks or connection must not start a late recording.
        startTask.cancel()
        endTask = Task {
            await startTask.value
            await stop?(cancelRequested)
            let completion = onEnd
            self.startTask = nil
            sessionID = nil
            stop = nil
            onEnd = nil
            endTask = nil
            completion?()
        }
    }
}
