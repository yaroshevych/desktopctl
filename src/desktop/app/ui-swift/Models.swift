import Foundation

struct JournalInput: Codable {
    var enabled: Bool
    var intervalSeconds: Int
    var outputDir: String
}

struct JournalOutput: Codable {
    var saved: Bool
    var enabled: Bool
    var intervalSeconds: Int
    var outputDir: String
}

enum PolicyMode: String, Codable, CaseIterable {
    case allowAll = "allow_all"
    case allowOnlySelected = "allow_only_selected"
    case allowAllExcept = "allow_all_except"

    var title: String {
        switch self {
        case .allowAll:
            "Allow all"
        case .allowOnlySelected:
            "Allow only selected"
        case .allowAllExcept:
            "Allow all, except"
        }
    }
}

struct AppPolicyInput: Codable {
    var policyMode: PolicyMode
    var apps: [String]
    var allowFullScreenCapture: Bool
    var clipboardAllowed: Bool
    var warning: String?
}

struct AppPolicyOutput: Codable {
    var saved: Bool
    var policyMode: PolicyMode
    var apps: [String]
    var allowFullScreenCapture: Bool
    var clipboardAllowed: Bool
}

struct LauncherInput: Codable {
    var agent: String
    var terminal: String
    var agents: [LauncherAgentOption]
    var renderKeyboardShortcuts: Bool
    var useNativeNotifications: Bool
    var openShortcut: LauncherShortcut
}

struct LauncherOutput: Codable {
    var saved: Bool
    var agent: String
    var terminal: String
    var renderKeyboardShortcuts: Bool
    var useNativeNotifications: Bool
    var openShortcut: LauncherShortcut
}

struct LauncherAgentOption: Codable, Hashable {
    var key: String
    var label: String
}

struct LauncherShortcut: Codable, Equatable {
    var keyCode: Int
    var modifiers: Int
}

struct SetupAccessInput: Codable {
    var cliInstalled: Bool
    var accessibilityGranted: Bool
    var screenRecordingGranted: Bool
    var cliSource: String?
    var candidateCliDirs: [String]
}
