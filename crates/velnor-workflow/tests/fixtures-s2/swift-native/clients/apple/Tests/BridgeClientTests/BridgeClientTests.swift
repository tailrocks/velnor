import XCTest

@testable import BridgeClient

final class BridgeClientTests: XCTestCase {
    func testBridgeCoreVersion() {
        XCTAssertEqual(bridgeCoreVersion(), 1)
    }
}
