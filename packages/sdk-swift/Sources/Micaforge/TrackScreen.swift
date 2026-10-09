#if canImport(SwiftUI)
import SwiftUI

extension View {
    /// Report this view as a screen when it appears.
    ///
    /// ```swift
    /// SettingsView()
    ///     .trackScreen("Settings")
    ///
    /// ProductView(product: product)
    ///     .trackScreen("Product", path: "/products/\(product.id)")
    /// ```
    ///
    /// A view that appears again while it is still the current screen (a tab switched back
    /// to, a sheet dismissed over it) is not counted twice. Uses the shared client, so call
    /// ``Micaforge/configure(siteKey:host:options:)`` first.
    public func trackScreen(_ name: String, path: String? = nil, props: Props = [:]) -> some View {
        modifier(TrackScreenModifier(name: name, path: path, props: props))
    }
}

/// The modifier behind ``SwiftUICore/View/trackScreen(_:path:props:)``.
struct TrackScreenModifier: ViewModifier {
    let name: String
    let path: String?
    let props: Props

    func body(content: Content) -> some View {
        content.onAppear {
            Micaforge.shared?.screenIfChanged(name, path: path, props: props)
        }
    }
}
#endif
