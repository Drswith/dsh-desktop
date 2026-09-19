import CoreServices
import Foundation
import XCTest
@testable import DSHLauncherCore

final class QuitRequestTests: XCTestCase {
    private func quitEvent(reason: OSType?) -> NSAppleEventDescriptor {
        let event = NSAppleEventDescriptor.appleEvent(
            withEventClass: AEEventClass(kCoreEventClass),
            eventID: AEEventID(kAEQuitApplication),
            targetDescriptor: nil,
            returnID: AEReturnID(kAutoGenerateReturnID),
            transactionID: AETransactionID(kAnyTransactionID)
        )
        if let reason {
            event.setAttribute(NSAppleEventDescriptor(enumCode: reason), forKeyword: AEKeyword(kAEQuitReason))
        }
        return event
    }

    func testLogoutRestartAndShutdownNeverWaitForConfirmation() {
        for reason in [kAELogOut, kAEReallyLogOut, kAEShowRestartDialog, kAERestart, kAEShowShutdownDialog, kAEShutDown] {
            XCTAssertTrue(QuitRequest.isSystemInitiated(quitEvent(reason: OSType(reason))), "reason \(reason)")
        }
    }

    func testUserQuitsStillConfirm() {
        XCTAssertFalse(QuitRequest.isSystemInitiated(nil), "⌘Q and menu items arrive without an Apple event")
        XCTAssertFalse(QuitRequest.isSystemInitiated(quitEvent(reason: nil)), "Dock and Activity Monitor quits carry no reason")
        let reopen = NSAppleEventDescriptor.appleEvent(
            withEventClass: AEEventClass(kCoreEventClass), eventID: AEEventID(kAEReopenApplication),
            targetDescriptor: nil, returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID)
        )
        reopen.setAttribute(NSAppleEventDescriptor(enumCode: OSType(kAELogOut)), forKeyword: AEKeyword(kAEQuitReason))
        XCTAssertFalse(QuitRequest.isSystemInitiated(reopen), "only quit events count")
    }
}
