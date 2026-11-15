import UIKit
import UniformTypeIdentifiers

/// Writes a line to GridCraft-log.txt (visible in Files: On My iPad > GridCraft).
func gcLog(_ message: String) {
    guard let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
    let url = docs.appendingPathComponent("GridCraft-log.txt")
    let line = "[swift] \(message)\n"
    if let handle = try? FileHandle(forWritingTo: url) {
        handle.seekToEndOfFile()
        handle.write(Data(line.utf8))
        try? handle.close()
    } else {
        try? line.write(to: url, atomically: true, encoding: .utf8)
    }
}

/// Owns the drawing view and everything that needs Apple's own screens: Files, alerts, printing,
/// the font picker.
///
/// Saving: Save As (and a workbook's first save) always shows Apple's Save to Files sheet. After
/// that, the app's Save and AutoSave write a working copy in Documents/Working, and this class
/// copies that file to the file the person chose, silently, about a second later.
final class GridCraftViewController: UIViewController, UIDocumentPickerDelegate, UIFontPickerViewControllerDelegate {

    private let metal = MetalView()
    private enum Mode { case open, save, picture }
    private var mode: Mode = .open

    /// File name -> the file the person chose in the Save to Files sheet.
    private var targets: [String: URL] = [:]
    /// File name -> when its working copy was last copied to the chosen file.
    private var lastCopied: [String: Date] = [:]
    private var mirrorTimer: Timer?

    override var prefersStatusBarHidden: Bool { true }
    override var prefersHomeIndicatorAutoHidden: Bool { true }

    private var workingFolder: URL? {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?.appendingPathComponent("Working", isDirectory: true)
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = UIColor(red: 0.11, green: 0.11, blue: 0.12, alpha: 1)

        metal.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(metal)
        let guide = view.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            metal.topAnchor.constraint(equalTo: guide.topAnchor),
            metal.bottomAnchor.constraint(equalTo: guide.bottomAnchor),
            metal.leadingAnchor.constraint(equalTo: guide.leadingAnchor),
            metal.trailingAnchor.constraint(equalTo: guide.trailingAnchor)
        ])

        metal.onOpenFiles = { [weak self] in self?.pick(.open) }
        metal.onPickPicture = { [weak self] in self?.pick(.picture) }
        metal.onSave = { [weak self] name, data in self?.handleSave(name: name, data: data) }
        metal.onOpenURL = { url in
            guard let u = URL(string: url), let scheme = u.scheme?.lowercased(),
                  ["http", "https", "mailto"].contains(scheme) else { return }
            UIApplication.shared.open(u)
        }
        metal.onCopyText = { UIPasteboard.general.string = $0 }
        metal.onCopyHTML = { html, text in
            UIPasteboard.general.setItems([["public.html": html, "public.utf8-plain-text": text]])
        }
        metal.onPasteText = { UIPasteboard.general.string }
        metal.onPasteHTML = {
            if let s = UIPasteboard.general.value(forPasteboardType: "public.html") as? String { return s }
            if let d = UIPasteboard.general.data(forPasteboardType: "public.html") { return String(data: d, encoding: .utf8) }
            return nil
        }
        metal.onPrint = { [weak self] data in self?.printPDF(data) }

        let nc = NotificationCenter.default
        nc.addObserver(forName: UIApplication.willResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.mirrorWorkingCopies()
            self?.metal.saveState()
            self?.metal.setFocused(false)
        }
        nc.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.metal.setFocused(true)
        }
        nc.addObserver(forName: .gcAddFonts, object: nil, queue: .main) { [weak self] _ in
            self?.showFontPicker()
        }

        mirrorTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            self?.mirrorWorkingCopies()
        }
    }

    // MARK: Fonts

    /// Apple's own font picker: the only way an app can use fonts installed on the iPad.
    private func showFontPicker() {
        guard presentedViewController == nil, view.window?.isKeyWindow ?? true else { return }
        var configuration = UIFontPickerViewController.Configuration()
        configuration.includeFaces = false
        let picker = UIFontPickerViewController(configuration: configuration)
        picker.delegate = self
        metal.suspendFocus = true
        present(picker, animated: true)
    }

    func fontPickerViewControllerDidPickFont(_ viewController: UIFontPickerViewController) {
        metal.suspendFocus = false
        guard let descriptor = viewController.selectedFontDescriptor else { return }
        let stored = SystemFonts.addPicked(descriptor)
        if presentedViewController != nil { dismiss(animated: true) }
        let family = (descriptor.object(forKey: .family) as? String) ?? "the font"
        if stored.isEmpty {
            message("Couldn’t add the font", "iPadOS did not let GridCraft copy “\(family)”. Some protected fonts can’t be used.")
        } else {
            message("Font added", "Added \(stored.count) style(s) of “\(family)”. Restart GridCraft once to use \(stored.count == 1 ? "it" : "them").")
        }
    }

    func fontPickerViewControllerDidCancel(_ viewController: UIFontPickerViewController) {
        metal.suspendFocus = false
    }

    // MARK: Opening files

    private func pick(_ m: Mode) {
        gcLog("pick requested: \(m); already presenting: \(presentedViewController != nil)")
        guard presentedViewController == nil else { return }
        mode = m
        let types: [UTType] = (m == .picture) ? [.image] : [.item]
        // A copy of the one chosen file: tapping the file returns it straight away.
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: types, asCopy: true)
        picker.allowsMultipleSelection = false
        picker.delegate = self
        metal.suspendFocus = true
        present(picker, animated: true) { gcLog("picker is on screen") }
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        gcLog("picker returned \(urls.count) item(s) in mode \(mode)")
        metal.suspendFocus = false
        switch mode {
        case .open:
            for url in urls { read(url) { name, data in self.metal.deliverOpened(name: name, data: data) } }
        case .picture:
            for url in urls { read(url) { name, data in self.metal.deliverPicture(name: name, data: data) } }
        case .save:
            if let u = urls.first {
                register(target: u)
                metal.saveResult(ok: true, name: u.lastPathComponent)
            } else {
                metal.saveResult(ok: false)
            }
        }
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        gcLog("picker cancelled in mode \(mode)")
        metal.suspendFocus = false
        if mode == .save { metal.saveResult(ok: false) }
    }

    private func read(_ url: URL, deliver: (String, Data) -> Void) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        do {
            let data = try Data(contentsOf: url)
            gcLog("read \(data.count) bytes from \(url.lastPathComponent)")
            deliver(url.lastPathComponent, data)
        } catch {
            gcLog("read failed: \(error.localizedDescription)")
            message("Couldn’t open the file", error.localizedDescription)
        }
    }

    // MARK: Saving

    /// Always Apple's own Save to Files sheet (Save As, a first save, or an export).
    private func handleSave(name: String, data: Data) {
        gcLog("save sheet requested: \(name), \(data.count) bytes")
        guard presentedViewController == nil else { metal.saveResult(ok: false); return }
        let safe = name.replacingOccurrences(of: "/", with: "_")
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let file = folder.appendingPathComponent(safe)
            try data.write(to: file)
            mode = .save
            let picker = UIDocumentPickerViewController(forExporting: [file], asCopy: true)
            picker.delegate = self
            metal.suspendFocus = true
            present(picker, animated: true)
        } catch {
            metal.suspendFocus = false
            metal.saveResult(ok: false)
            message("Couldn’t prepare the file", error.localizedDescription)
        }
    }

    /// Remembers the file the person chose, so later saves can update it without asking.
    private func register(target url: URL) {
        let name = url.lastPathComponent
        guard name.lowercased().hasSuffix(".xlsx") else { return }
        _ = url.startAccessingSecurityScopedResource()   // kept open so later saves can write
        targets[name] = url
        lastCopied[name] = Date()
        gcLog("silent saves will update \(name)")
    }

    /// Copies each working copy that changed since the last copy to the file the person chose.
    private func mirrorWorkingCopies() {
        guard let folder = workingFolder else { return }
        for (name, url) in targets {
            let work = folder.appendingPathComponent(name)
            guard let attrs = try? FileManager.default.attributesOfItem(atPath: work.path),
                  let modified = attrs[.modificationDate] as? Date else { continue }
            if modified <= (lastCopied[name] ?? .distantPast) { continue }
            guard let data = try? Data(contentsOf: work) else { continue }

            var coordError: NSError?
            var writeError: Error?
            NSFileCoordinator().coordinate(writingItemAt: url, options: .forReplacing, error: &coordError) { writeURL in
                do { try data.write(to: writeURL) } catch { writeError = error }
            }
            if let e = (coordError as Error?) ?? writeError {
                gcLog("could not update \(name): \(e.localizedDescription)")
                targets[name] = nil
                message("Couldn’t update “\(name)”",
                        "Your changes are kept in Files › On My iPad › GridCraft › Working. Use Save As to save a copy where you want it.\n\n\(e.localizedDescription)")
            } else {
                lastCopied[name] = modified
                gcLog("updated \(name) (\(data.count) bytes)")
            }
        }
    }

    // MARK: Printing and messages

    private func printPDF(_ data: Data) {
        let controller = UIPrintInteractionController.shared
        let info = UIPrintInfo(dictionary: nil)
        info.outputType = .general
        info.jobName = "GridCraft"
        controller.printInfo = info
        controller.printingItem = data
        controller.present(from: metal.bounds, in: metal, animated: true, completionHandler: nil)
    }

    /// Shown a moment later, so it still appears while a picker is closing.
    private func message(_ title: String, _ text: String) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
            guard let self = self, self.presentedViewController == nil else { return }
            let a = UIAlertController(title: title, message: text, preferredStyle: .alert)
            a.addAction(UIAlertAction(title: "OK", style: .default))
            self.present(a, animated: true)
        }
    }
}
