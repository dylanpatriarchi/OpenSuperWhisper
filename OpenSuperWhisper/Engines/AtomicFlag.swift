import Foundation

/// Thread-safe boolean shared between a transcription's caller and the thread
/// actually running the engine.
///
/// It is a `class` on purpose: `WhisperEngine` hands a pointer to one of these
/// into whisper.cpp's C abort callback, which requires a stable address that
/// outlives any single transcription.
final class AtomicFlag {
    private let lock = NSLock()
    private var _isSet: Bool

    init(_ initialValue: Bool = false) {
        _isSet = initialValue
    }

    var isSet: Bool {
        get {
            lock.lock()
            defer { lock.unlock() }
            return _isSet
        }
        set {
            lock.lock()
            defer { lock.unlock() }
            _isSet = newValue
        }
    }
}
