import Foundation

@MainActor
class TranscriptionService: ObservableObject {
    static let shared = TranscriptionService()

    @Published private(set) var isTranscribing = false
    @Published private(set) var transcribedText = ""
    @Published private(set) var currentSegment = ""
    @Published private(set) var isLoading = false
    @Published private(set) var progress: Float = 0.0
    @Published private(set) var isConverting = false
    @Published private(set) var conversionProgress: Float = 0.0

    /// Boxes the task so pending work can be compared by identity (`===`);
    /// `Task` is a struct and has no identity of its own.
    private final class TranscriptionTaskBox {
        let task: Task<String, Error>
        init(_ task: Task<String, Error>) { self.task = task }
    }

    private var currentEngine: TranscriptionEngine?

    /// Most recently queued transcription. New work chains onto it so a single
    /// engine — and the whisper context inside it — is never entered twice
    /// concurrently by the indicator and queue flows.
    private var transcriptionTask: TranscriptionTaskBox?

    /// Set by `cancelTranscription()` and cleared only when the next
    /// transcription starts. The canceller must not clear it itself, or the
    /// in-flight run would never observe the cancellation.
    private var isCancelled = false

    /// Discriminates overlapping `loadEngine()` calls so a slow earlier load
    /// cannot install its engine over a newer one.
    private var engineLoadGeneration = 0

    init() {
        loadEngine()
    }

    /// Whether a transcription can start right now. Loading a multi-gigabyte
    /// model takes seconds; until it completes `transcribeAudio` throws
    /// `contextInitializationFailed`, so callers should check this and tell the
    /// user rather than discarding the dictation silently.
    var isEngineReady: Bool {
        currentEngine != nil && !isLoading
    }

    func cancelTranscription() {
        isCancelled = true
        currentEngine?.cancelTranscription()
        transcriptionTask?.task.cancel()

        isTranscribing = false
        currentSegment = ""
        progress = 0.0
    }

    private func loadEngine() {
        let selectedEngine = AppPreferences.shared.selectedEngine
        print("Loading engine: \(selectedEngine)")

        engineLoadGeneration &+= 1
        let generation = engineLoadGeneration
        isLoading = true

        Task.detached(priority: .userInitiated) { [weak self] in
            let engine: TranscriptionEngine?

            if selectedEngine == "fluidaudio" {
                engine = await FluidAudioEngine()
            } else {
                engine = await WhisperEngine()
            }

            do {
                try await engine?.initialize()

                await MainActor.run {
                    guard let self, self.engineLoadGeneration == generation else { return }
                    self.currentEngine = engine
                    self.isLoading = false
                    print("Engine loaded: \(selectedEngine)")
                }
            } catch {
                await MainActor.run {
                    guard let self, self.engineLoadGeneration == generation else { return }
                    // Drop any previously loaded engine: preferences now point at
                    // the model that failed to load, so keeping the old one would
                    // silently transcribe with a different model than the one
                    // selected. Better to report "not ready".
                    self.currentEngine = nil
                    self.isLoading = false
                    print("Failed to load engine: \(error)")
                }
            }
        }
    }

    func reloadEngine() {
        loadEngine()
    }

    func reloadModel(with path: String) {
        if AppPreferences.shared.selectedEngine == "whisper" {
            AppPreferences.shared.selectedWhisperModelPath = path
            reloadEngine()
        }
    }

    func transcribeAudio(url: URL, settings: Settings) async throws -> String {
        // Chain onto whatever is already queued. Reading the predecessor and
        // installing our own box happen with no `await` in between, so on the
        // main actor two callers can never both observe an idle engine.
        let predecessor = transcriptionTask
        let task = Task<String, Error> { [weak self] in
            _ = try? await predecessor?.task.value
            guard let self else { throw CancellationError() }
            return try await self.performTranscription(url: url, settings: settings)
        }
        let box = TranscriptionTaskBox(task)
        transcriptionTask = box

        defer {
            // Only clear the slot if it is still ours; a later caller may have
            // already chained onto us and installed its own box.
            if transcriptionTask === box {
                transcriptionTask = nil
            }
        }

        do {
            return try await task.value
        } catch is CancellationError {
            throw TranscriptionError.cancelled
        }
    }

    private func performTranscription(url: URL, settings: Settings) async throws -> String {
        progress = 0.0
        conversionProgress = 0.0
        isConverting = true
        isTranscribing = true
        transcribedText = ""
        currentSegment = ""
        isCancelled = false

        defer {
            isTranscribing = false
            isConverting = false
            currentSegment = ""
            if !isCancelled {
                progress = 1.0
            }
        }

        guard let engine = currentEngine else {
            throw TranscriptionError.contextInitializationFailed
        }

        // Setup progress callback for engines
        if let whisperEngine = engine as? WhisperEngine {
            whisperEngine.onProgressUpdate = { [weak self] newProgress in
                Task { @MainActor in
                    guard let self = self, !self.isCancelled else { return }
                    self.progress = newProgress
                }
            }
        } else if let fluidEngine = engine as? FluidAudioEngine {
            fluidEngine.onProgressUpdate = { [weak self] newProgress in
                Task { @MainActor in
                    guard let self = self, !self.isCancelled else { return }
                    self.progress = newProgress
                }
            }
        }

        guard !isCancelled else { throw CancellationError() }

        let work = Task.detached(priority: .userInitiated) {
            try Task.checkCancellation()
            return try await engine.transcribeAudio(url: url, settings: settings)
        }

        let result = try await work.value

        // Publish only after confirming the run was not cancelled, so a
        // cancelled transcription never flashes its text into the UI.
        try Task.checkCancellation()
        guard !isCancelled else { throw CancellationError() }

        transcribedText = result
        progress = 1.0

        return result
    }
}

enum TranscriptionError: Error {
    case contextInitializationFailed
    case audioConversionFailed
    case processingFailed
    case cancelled
}
