// Standalone harness; excluded from the production Swift source list.
import Foundation

@main
struct LauncherBridgeTests {
    static func main() {
        runLauncherModelRegressionTests()
        print("Launcher model regression tests passed")
    }
}
