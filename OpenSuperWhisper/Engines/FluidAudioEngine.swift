import Foundation
import AVFoundation
import FluidAudio

class FluidAudioEngine: TranscriptionEngine {
    var engineName: String { "FluidAudio" }
    
    private var asrManager: AsrManager?
    private var asrModels: AsrModels?
    /// Written by `cancelTranscription()` on the main actor, read from the
    /// background thread running the transcription — needs real synchronization.
    private let cancelFlag = AtomicFlag()
    private var transcriptionTask: Task<String, Error>?
    private var progressTask: Task<Void, Never>?
    
    var onProgressUpdate: ((Float) -> Void)?
    
    var isModelLoaded: Bool {
        asrManager != nil
    }
    
    func initialize() async throws {
        let versionString = AppPreferences.shared.fluidAudioModelVersion
        let version: AsrModelVersion = versionString == "v2" ? .v2 : .v3
        
        let models = try await AsrModels.downloadAndLoad(version: version)
        let manager = AsrManager(config: .default)
        try await manager.loadModels(models)
        
        asrManager = manager
        asrModels = models
    }
    
    func transcribeAudio(url: URL, settings: Settings) async throws -> String {
        guard let asrManager = asrManager else {
            throw TranscriptionError.contextInitializationFailed
        }
        
        cancelFlag.isSet = false

        // Notify start
        onProgressUpdate?(0.02)

        guard !cancelFlag.isSet else {
            throw CancellationError()
        }
        
        // Start progress monitoring task using FluidAudio's transcriptionProgressStream
        let onProgress = onProgressUpdate
        progressTask?.cancel()
        progressTask = Task { [weak self] in
            guard let self = self else { return }
            
            do {
                // Get the real progress stream from FluidAudio
                let progressStream = await asrManager.transcriptionProgressStream
                
                for try await progress in progressStream {
                    guard !Task.isCancelled, !self.cancelFlag.isSet else { break }
                    
                    // FluidAudio reports 0.0-1.0, we map to 0.05-0.95
                    let scaledProgress = 0.05 + Float(progress) * 0.90
                    
                    await MainActor.run {
                        onProgress?(scaledProgress)
                    }
                }
            } catch {
                // Stream finished or error
            }
        }
        
        defer {
            progressTask?.cancel()
            progressTask = nil
        }
        
        // Perform actual transcription - FluidAudio will emit progress automatically.
        // Run it as a tracked task: `transcriptionTask` was previously declared and
        // cancelled but never assigned, which made cancellation a silent no-op here
        // and left a full ANE/CPU workload running after the user cancelled.
        let task = Task<String, Error> {
            // FluidAudio 0.15.x requires an explicit decoder state per transcription.
            var decoderState = try TdtDecoderState(decoderLayers: await asrManager.decoderLayerCount)
            try Task.checkCancellation()
            let result = try await asrManager.transcribe(url, decoderState: &decoderState)
            try Task.checkCancellation()
            return result.text
        }
        transcriptionTask = task
        defer { transcriptionTask = nil }

        let rawText = try await task.value

        guard !cancelFlag.isSet else {
            throw CancellationError()
        }

        // Finalize
        onProgressUpdate?(0.95)

        let processedText = rawText.trimmingCharacters(in: .whitespacesAndNewlines)

        onProgressUpdate?(1.0)

        return processedText
    }
    
    func cancelTranscription() {
        cancelFlag.isSet = true
        progressTask?.cancel()
        progressTask = nil
        transcriptionTask?.cancel()
        transcriptionTask = nil
    }
    
    func getSupportedLanguages() -> [String] {
        LanguageUtil.supportedLanguages(
            engine: "fluidaudio",
            fluidAudioModelVersion: AppPreferences.shared.fluidAudioModelVersion
        )
    }
}

