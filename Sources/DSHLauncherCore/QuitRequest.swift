import CoreServices
import Foundation

/// Classifies the Apple event behind a quit request.
public enum QuitRequest {
    /// Logout, restart and shutdown deliver a quit event carrying a reason; a Dock or
    /// Activity Monitor quit carries none, and ⌘Q is no Apple event at all.
    public static func isSystemInitiated(_ event: NSAppleEventDescriptor?) -> Bool {
        guard let event,
              event.eventClass == AEEventClass(kCoreEventClass),
              event.eventID == AEEventID(kAEQuitApplication),
              let reason = event.attributeDescriptor(forKeyword: AEKeyword(kAEQuitReason)) else { return false }
        let systemReasons = [kAELogOut, kAEReallyLogOut, kAEShowRestartDialog, kAERestart, kAEShowShutdownDialog, kAEShutDown]
        return systemReasons.map { OSType($0) }.contains(reason.enumCodeValue)
    }
}
