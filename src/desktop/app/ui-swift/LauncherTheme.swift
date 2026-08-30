import AppKit
import SwiftUI

// Small, native macOS token set. Keep this file independent from launcher state
// so platform UI can reuse it without adding bridge or model work.
internal enum LauncherTheme {
    internal static let referenceWidth: CGFloat = 700

    internal enum Radius {
        internal static let panel: CGFloat = 10
        internal static let row: CGFloat = 10
    }

    internal enum Spacing {
        internal static let xxs: CGFloat = 2
        internal static let xs: CGFloat = 4
        internal static let sm: CGFloat = 6
        internal static let md: CGFloat = 8
        internal static let lg: CGFloat = 10
        internal static let xl: CGFloat = 12
        internal static let xxl: CGFloat = 20
    }

    internal static let headerHeight: CGFloat = 64
    internal static let rowIconSize: CGFloat = 24
    internal static let controlHeight: CGFloat = 32

    internal static func panelEdge(colorScheme: ColorScheme) -> Color {
        colorScheme == .dark
            ? Color.white.opacity(0.22)
            : Color.black.opacity(0.28)
    }

    internal static let textPrimary = Color(nsColor: .labelColor)
    internal static let textSecondary = Color(nsColor: .secondaryLabelColor)
    internal static let textTertiary = Color(nsColor: .tertiaryLabelColor)

    internal static func panelScrim(
        colorScheme: ColorScheme,
        reduceTransparency: Bool
    ) -> Color {
        if reduceTransparency {
            return colorScheme == .dark
                ? Color(nsColor: .windowBackgroundColor)
                : Color(nsColor: .controlBackgroundColor)
        }
        return colorScheme == .dark
            ? Color.black.opacity(0.22)
            : Color.white.opacity(0.32)
    }

    internal static func selection(
        colorScheme: ColorScheme,
        reduceTransparency: Bool
    ) -> Color {
        if reduceTransparency {
            return colorScheme == .dark
                ? Color(nsColor: .selectedContentBackgroundColor)
                : Color(nsColor: .unemphasizedSelectedContentBackgroundColor)
        }
        return colorScheme == .dark
            ? Color.white.opacity(0.12)
            : Color.black.opacity(0.07)
    }

    internal static func hover(
        colorScheme: ColorScheme,
        reduceTransparency: Bool
    ) -> Color {
        if reduceTransparency {
            return colorScheme == .dark
                ? Color.white.opacity(0.16)
                : Color.black.opacity(0.08)
        }
        return colorScheme == .dark
            ? Color.white.opacity(0.08)
            : Color.black.opacity(0.045)
    }

    internal static func keyCapBackground(
        colorScheme: ColorScheme,
        reduceTransparency: Bool
    ) -> Color {
        if reduceTransparency {
            return colorScheme == .dark
                ? Color(nsColor: .controlBackgroundColor)
                : Color(nsColor: .windowBackgroundColor)
        }
        return colorScheme == .dark
            ? Color.white.opacity(0.10)
            : Color.black.opacity(0.055)
    }

    internal static func interactionAnimation(reduceMotion: Bool) -> Animation? {
        reduceMotion ? nil : .easeOut(duration: 0.12)
    }
}

// NSVisualEffectView is cheaper and more native than a SwiftUI blur. Keep one
// AppKit view alive; only update properties when SwiftUI state changes.
internal struct LauncherVisualEffectView: NSViewRepresentable {
    internal var material: NSVisualEffectView.Material = .hudWindow
    internal var blendingMode: NSVisualEffectView.BlendingMode = .behindWindow
    internal var state: NSVisualEffectView.State = .active

    internal func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = state
        view.wantsLayer = true
        return view
    }

    internal func updateNSView(_ view: NSVisualEffectView, context: Context) {
        if view.material != material { view.material = material }
        if view.blendingMode != blendingMode { view.blendingMode = blendingMode }
        if view.state != state { view.state = state }
    }
}

internal struct LauncherKeyCap: View {
    internal let title: String

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    internal init(_ title: String) {
        self.title = title
    }

    internal init(title: String) {
        self.title = title
    }

    internal var body: some View {
        Text(title)
            .font(.system(size: 11, weight: .semibold, design: .rounded))
            .foregroundStyle(LauncherTheme.textSecondary)
            .lineLimit(1)
            .frame(minWidth: 22, minHeight: 20)
            .padding(.horizontal, LauncherTheme.Spacing.sm)
            .background(
                Capsule().fill(
                    LauncherTheme.keyCapBackground(
                        colorScheme: colorScheme,
                        reduceTransparency: reduceTransparency
                    )
                )
            )
            .overlay(
                Capsule().stroke(LauncherTheme.textTertiary.opacity(0.24), lineWidth: 0.5)
            )
            .accessibilityLabel("Keyboard shortcut \(title)")
    }
}

internal struct LauncherBarButton<Label: View>: View {
    private let action: () -> Void
    private let label: () -> Label
    @State private var isHovered = false

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    internal init(
        action: @escaping () -> Void,
        @ViewBuilder label: @escaping () -> Label
    ) {
        self.action = action
        self.label = label
    }

    internal var body: some View {
        Button(action: action) {
            label()
                .frame(minHeight: LauncherTheme.controlHeight)
                .padding(.horizontal, LauncherTheme.Spacing.md)
                .contentShape(
                    RoundedRectangle(
                        cornerRadius: LauncherTheme.Radius.row,
                        style: .continuous
                    )
                )
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: LauncherTheme.Radius.row, style: .continuous)
                .fill(
                    isHovered
                        ? LauncherTheme.hover(
                            colorScheme: colorScheme,
                            reduceTransparency: reduceTransparency
                        )
                        : .clear
                )
        )
        .onHover { isHovered = $0 }
        .animation(
            LauncherTheme.interactionAnimation(reduceMotion: reduceMotion),
            value: isHovered
        )
    }
}

internal extension LauncherBarButton where Label == SwiftUI.Label<Text, Image> {
    init(title: String, systemImage: String, action: @escaping () -> Void) {
        self.init(action: action) {
            Label(title, systemImage: systemImage)
        }
    }
}

internal struct LauncherSectionHeader: View {
    internal let title: String
    internal let systemImage: String?

    internal init(_ title: String, systemImage: String? = nil) {
        self.title = title
        self.systemImage = systemImage
    }

    internal init(title: String, systemImage: String? = nil) {
        self.title = title
        self.systemImage = systemImage
    }

    internal var body: some View {
        HStack(spacing: LauncherTheme.Spacing.sm) {
            if let systemImage {
                Image(systemName: systemImage)
                    .accessibilityHidden(true)
            }
            Text(title)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(LauncherTheme.textSecondary)
                .textCase(.uppercase)
            Spacer(minLength: 0)
        }
        .frame(height: LauncherTheme.Spacing.xxl)
        .accessibilityAddTraits(.isHeader)
    }
}
