import SwiftUI

extension Notification.Name {
    static let gcAddFonts = Notification.Name("GridCraftAddFonts")
}

@main
struct GridCraftApp: App {
    init() {
        // Before any window exists, so GridCraft finds the iPad's fonts when it starts.
        SystemFonts.prepare()
    }

    var body: some Scene {
        WindowGroup {
            ZStack {
                Color(red: 0.11, green: 0.11, blue: 0.12).ignoresSafeArea()
                GridCraftHost()
            }
            .statusBarHidden(true)
        }
        .commands {
            CommandMenu("Fonts") {
                Button("Add Installed Fonts…") {
                    NotificationCenter.default.post(name: .gcAddFonts, object: nil)
                }
                .keyboardShortcut("f", modifiers: [.command, .option, .shift])
            }
        }
    }
}

struct GridCraftHost: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> GridCraftViewController { GridCraftViewController() }
    func updateUIViewController(_ vc: GridCraftViewController, context: Context) {}
}
