import AppKit
import Carbon.HIToolbox
import UserNotifications
import WebKit

// Turtwig: a thin native shell drawn like super.engineering. The sidebar and
// the main chrome (toolbar, tab strip, branch bar, terminal tabs) are pages the
// box's dashboard serves; the chat (claude.ai) and the terminal (the box's
// ttyd) are native web views placed over holes the main page reports.
// Everything heavy runs on the box, reached through an ssh tunnel.

let sshHost = UserDefaults.standard.string(forKey: "sshHost") ?? "coder.tj-agents"
let dashPort = 18790
let ttyPort = 18791
let agentBoxDir = (NSHomeDirectory() as NSString).appendingPathComponent("dev/turtwig")
let sidebarWidth: CGFloat = 339
let chromeColor = NSColor(srgbRed: 0x21 / 255, green: 0x21 / 255, blue: 0x22 / 255, alpha: 1)
let cardColor = NSColor(srgbRed: 0x1e / 255, green: 0x1e / 255, blue: 0x1f / 255, alpha: 1)

final class Tunnel {
    private var process: Process?
    private var stopping = false

    func start() {
        stopping = false
        launch()
    }

    func stop() {
        stopping = true
        process?.terminate()
    }

    private func launch() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
        p.arguments = [
            "-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=3",
            "-L", "\(dashPort):127.0.0.1:8790", "-L", "\(ttyPort):127.0.0.1:8791", sshHost,
        ]
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        p.terminationHandler = { [weak self] _ in
            guard let self, !self.stopping else { return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { self.launch() }
        }
        try? p.run()
        process = p
    }
}

// A web view whose empty title-bar area drags the window, like a native title bar.
final class ChromeWebView: WKWebView {
    var dragBand: CGFloat = 44
    var hits: [CGRect] = []

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        let y = isFlipped ? p.y : bounds.height - p.y
        if y < dragBand, !hits.contains(where: { $0.contains(CGPoint(x: p.x, y: y)) }) {
            if event.clickCount == 2 { window?.zoom(nil) } else { window?.performDrag(with: event) }
            return
        }
        super.mouseDown(with: event)
    }
}

final class Root: NSView {
    override var isFlipped: Bool { true }
}

final class Shell: NSObject, NSApplicationDelegate, NSWindowDelegate, WKScriptMessageHandler, WKUIDelegate, WKNavigationDelegate {
    var window: NSWindow!
    var root: Root!
    var side: ChromeWebView!
    var main: ChromeWebView!
    var chat: WKWebView!
    var term: WKWebView!
    var statusItem: NSStatusItem!
    let tunnel = Tunnel()
    var lastNeeds = 0
    var selectedTerm: String?
    var current: [String: Any]?
    var hotKey: EventHotKeyRef?
    var sidebarShown = true
    var chatHole = CGRect.zero
    var termHole = CGRect.zero

    func applicationDidFinishLaunching(_ notification: Notification) {
        tunnel.start()

        side = makeChrome()
        main = makeChrome()
        chat = makeWeb(WKWebView.self, bridged: false)
        term = makeWeb(WKWebView.self, bridged: false)
        side.hits = [CGRect(x: 88, y: 12, width: 28, height: 22)]
        for v in [chat!, term!] {
            v.wantsLayer = true
            v.layer?.cornerRadius = 12
            v.layer?.masksToBounds = true
        }
        // Rounded only at the bottom, where the chat hole meets its card's corners.
        chat.layer?.maskedCorners = [.layerMinXMinYCorner, .layerMaxXMinYCorner]
        term.layer?.maskedCorners = []

        root = Root()
        root.wantsLayer = true
        root.layer?.backgroundColor = chromeColor.cgColor
        for v in [side!, main!, chat!, term!] { root.addSubview(v) }

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1500, height: 940),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.title = "Turtwig"
        window.backgroundColor = chromeColor
        window.minSize = NSSize(width: 900, height: 560)
        window.contentView = root
        window.delegate = self
        window.setFrameAutosaveName("TurtwigWindow")
        if window.frame.width < 600 { window.center() }
        window.makeKeyAndOrderFront(nil)
        layout()

        chat.load(URLRequest(url: URL(string: "https://claude.ai/code")!))
        chat.isHidden = true
        term.isHidden = true
        loadLocal()

        // Keep the box's copy of the Mac's sidebar, dispatcher state, skills, and
        // memory fresh while the app is open.
        Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in self?.pushEnv() }
        pushEnv()

        setupMenu()
        setupStatusItem()
        setupHotKey()
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { _, _ in }
        NSApp.activate(ignoringOtherApps: true)
        if let dir = ProcessInfo.processInfo.environment["AGENTBOX_SNAPSHOT_DIR"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { self.snapshot(to: dir) }
        }
    }

    // MARK: layout

    func layout(animated: Bool = false) {
        let b = root.bounds
        let sw: CGFloat = sidebarShown ? sidebarWidth : 0
        let apply = {
            self.side.frame = CGRect(x: 0, y: 0, width: sidebarWidth, height: b.height)
            self.side.alphaValue = self.sidebarShown ? 1 : 0
            self.main.frame = CGRect(x: sw, y: 0, width: b.width - sw, height: b.height)
            self.place()
        }
        if animated {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.2
                ctx.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
                ctx.allowsImplicitAnimation = true
                apply()
            }
        } else {
            apply()
        }
        positionTrafficLights()
    }

    func place() {
        let o = main.frame.origin
        chat.frame = chatHole.offsetBy(dx: o.x, dy: o.y)
        term.frame = termHole.offsetBy(dx: o.x, dy: o.y)
    }

    // Center the traffic lights in the 44 pt title band, as super.engineering does.
    func positionTrafficLights() {
        guard let close = window.standardWindowButton(.closeButton), let bar = close.superview else { return }
        var x: CGFloat = 20
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            guard let btn = window.standardWindowButton(kind) else { continue }
            let y = bar.isFlipped ? 22 - btn.frame.height / 2 : bar.frame.height - 22 - btn.frame.height / 2
            btn.setFrameOrigin(NSPoint(x: x, y: y))
            x += 20
        }
    }

    func windowDidResize(_ notification: Notification) { layout() }
    func windowDidEndLiveResize(_ notification: Notification) { positionTrafficLights() }
    func windowDidBecomeKey(_ notification: Notification) { positionTrafficLights() }
    func windowDidExitFullScreen(_ notification: Notification) { layout() }

    @objc func toggleSidebar() {
        sidebarShown.toggle()
        main.evaluateJavaScript("window.setSidebarHidden && setSidebarHidden(\(!sidebarShown))")
        layout(animated: true)
    }

    // MARK: web views

    func makeChrome() -> ChromeWebView {
        let w = makeWeb(ChromeWebView.self, bridged: true)
        w.underPageBackgroundColor = chromeColor
        return w
    }

    func makeWeb<T: WKWebView>(_ kind: T.Type, bridged: Bool) -> T {
        let config = WKWebViewConfiguration()
        if bridged { config.userContentController.add(self, name: "agentbox") }
        config.websiteDataStore = .default()
        let w = T(frame: .zero, configuration: config)
        w.uiDelegate = self
        w.navigationDelegate = self
        w.underPageBackgroundColor = cardColor
        return w
    }

    func loadLocal() {
        side.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/")!))
        main.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/main")!))
    }

    // The tunnel takes a moment to come up; retry the local pages until it does.
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if webView === side || webView === main {
            let url = URL(string: webView === side ? "http://127.0.0.1:\(dashPort)/" : "http://127.0.0.1:\(dashPort)/main")!
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { webView.load(URLRequest(url: url)) }
        } else if webView === term, let s = selectedTerm {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.attachTerminal(s) }
        }
    }

    func attachTerminal(_ session: String) {
        selectedTerm = session
        term.load(URLRequest(url: URL(string: "http://127.0.0.1:\(ttyPort)/?arg=\(session)")!))
    }

    // Debugging aid: AGENTBOX_SNAPSHOT_DIR=<dir> writes each view as a PNG.
    func snapshot(to dir: String) {
        let views: [(String, WKWebView)] = [("side", side), ("main", main), ("chat", chat), ("term", term)]
        for (name, view) in views {
            view.takeSnapshot(with: nil) { image, _ in
                guard let image, let tiff = image.tiffRepresentation,
                      let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else { return }
                try? png.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
            }
        }
    }

    // JavaScript alert/confirm from the app's own pages, as sheets.
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        let a = NSAlert()
        a.messageText = message
        a.beginSheetModal(for: window) { _ in completionHandler() }
    }

    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let a = NSAlert()
        a.messageText = message
        a.addButton(withTitle: "OK")
        a.addButton(withTitle: "Cancel")
        a.beginSheetModal(for: window) { completionHandler($0 == .alertFirstButtonReturn) }
    }

    // Links meant for a new window, and app URLs (cursor://, vscode://), go to macOS.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url {
            if webView === chat, url.host?.hasSuffix("claude.ai") == true || url.host?.contains("google") == true
                || url.host?.contains("apple") == true {
                webView.load(navigationAction.request)
            } else {
                NSWorkspace.shared.open(url)
            }
        }
        return nil
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url, let scheme = url.scheme else {
            return decisionHandler(.allow)
        }
        if !["http", "https", "about", "data", "blob"].contains(scheme) {
            NSWorkspace.shared.open(url)
            return decisionHandler(.cancel)
        }
        if webView !== chat, webView !== term, navigationAction.navigationType == .linkActivated,
           url.host != "127.0.0.1" {
            NSWorkspace.shared.open(url)
            return decisionHandler(.cancel)
        }
        decisionHandler(.allow)
    }

    // MARK: messages from the pages

    func js(_ obj: Any) -> String {
        (try? JSONSerialization.data(withJSONObject: obj, options: [.fragmentsAllowed]))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "null"
    }

    func rect(_ v: Any?) -> CGRect {
        guard let d = v as? [String: Any] else { return .zero }
        func n(_ k: String) -> CGFloat { CGFloat((d[k] as? NSNumber)?.doubleValue ?? 0) }
        return CGRect(x: n("x"), y: n("y"), width: n("w"), height: n("h"))
    }

    func show(chat c: [String: Any]?) {
        if let u = c?["rc_url"] as? String, let url = URL(string: u) {
            chat.isHidden = false
            if chat.url?.absoluteString != u { chat.load(URLRequest(url: url)) }
        } else {
            chat.isHidden = true
        }
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any], let type = body["type"] as? String else { return }
        switch type {
        case "layout":
            chatHole = rect(body["chat"])
            termHole = rect(body["term"])
            main.dragBand = rect(body["toolbar"]).maxY
            main.hits = (body["hits"] as? [Any] ?? []).map(rect)
            place()
        case "select":
            guard let sel = body["sel"] as? [String: Any] else { return }
            current = sel
            let first = (sel["chats"] as? [[String: Any]])?.first
            main.evaluateJavaScript("setSel(\(js(sel)), \(js(first?["id"] ?? NSNull())))")
            show(chat: first)
            if let t = sel["term"] as? String {
                term.isHidden = false
                if t != selectedTerm { attachTerminal(t) }
            } else {
                selectedTerm = nil
                term.isHidden = true
            }
        case "tab":
            show(chat: body["chat"] as? [String: Any])
        case "newchat":
            side.evaluateJavaScript("openNewChat(\(js(body["dir"] ?? "")))")
            window.makeFirstResponder(side)
        case "toggleSidebar":
            toggleSidebar()
        case "open":
            if let u = body["url"] as? String, let url = URL(string: u) { NSWorkspace.shared.open(url) }
        case "send":
            if let id = body["chat"] as? String, let text = body["text"] as? String { sendToChat(id, text) }
        case "status":
            updateStatus(body["needs"] as? Int ?? 0)
        case "run":
            if let args = body["args"] as? [String], let id = body["reqId"] as? String {
                runLocal(args, reqId: id, reply: message.webView ?? main)
            }
        default:
            break
        }
    }

    func sendToChat(_ id: String, _ text: String) {
        var req = URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/api/chats/send")!)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = js(["id": id, "text": text]).data(using: .utf8)
        URLSession.shared.dataTask(with: req).resume()
    }

    // Runs one of agent-box's own scripts on the Mac (swap, test locally).
    func runLocal(_ args: [String], reqId: String, reply: WKWebView) {
        guard let tool = args.first, ["chat", "wip-restore", "env-push"].contains(tool) else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: (agentBoxDir as NSString).appendingPathComponent("bin/" + tool))
        p.arguments = Array(args.dropFirst())
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "\(NSHomeDirectory())/.local/bin:\(NSHomeDirectory())/dev/redo-monorepo-worktrees/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"
        p.environment = env
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        p.terminationHandler = { proc in
            let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            let payload = self.js(["reqId": reqId, "code": proc.terminationStatus, "output": text])
            DispatchQueue.main.async {
                reply.evaluateJavaScript("window.agentboxResult && window.agentboxResult(\(payload))")
                self.side.evaluateJavaScript("window.refreshSidebar && window.refreshSidebar()")
            }
        }
        do { try p.run() } catch {
            reply.evaluateJavaScript("window.agentboxResult && window.agentboxResult({reqId: '\(reqId)', code: 127, output: 'could not start \(tool)'})")
        }
    }

    func pushEnv() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: (agentBoxDir as NSString).appendingPathComponent("bin/env-push"))
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    // MARK: status, menus, hot key

    func updateStatus(_ needs: Int) {
        statusItem.button?.title = needs > 0 ? "✋\(needs)" : "◇"
        NSApp.dockTile.badgeLabel = needs > 0 ? "\(needs)" : nil
        if needs > lastNeeds {
            let content = UNMutableNotificationContent()
            content.title = "Turtwig"
            content.body = needs == 1 ? "1 thing needs you" : "\(needs) things need you"
            UNUserNotificationCenter.current().add(
                UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
        lastNeeds = needs
    }

    func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.title = "◇"
        statusItem.button?.target = self
        statusItem.button?.action = #selector(showWindow)
    }

    @objc func showWindow() {
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    func applicationWillTerminate(_ notification: Notification) {
        tunnel.stop()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        window.makeKeyAndOrderFront(nil)
        return true
    }

    @objc func reloadAll() {
        loadLocal()
        chat.reload()
        if let s = selectedTerm { attachTerminal(s) }
    }

    @objc func focusSide() { window.makeFirstResponder(side) }
    @objc func focusChat() { window.makeFirstResponder(chat) }
    @objc func focusTerm() { window.makeFirstResponder(term) }

    func setupMenu() {
        let menu = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Turtwig", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        menu.addItem(appItem)

        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        edit.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        edit.addItem(.separator())
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        menu.addItem(editItem)

        let viewItem = NSMenuItem()
        let view = NSMenu(title: "View")
        view.addItem(withTitle: "Toggle Sidebar", action: #selector(toggleSidebar), keyEquivalent: "\\")
        view.addItem(withTitle: "Sidebar", action: #selector(focusSide), keyEquivalent: "1")
        view.addItem(withTitle: "Chat", action: #selector(focusChat), keyEquivalent: "2")
        view.addItem(withTitle: "Terminal", action: #selector(focusTerm), keyEquivalent: "3")
        view.addItem(withTitle: "Reload", action: #selector(reloadAll), keyEquivalent: "r")
        view.items.forEach { $0.target = self }
        viewItem.submenu = view
        menu.addItem(viewItem)
        NSApp.mainMenu = menu
    }

    // ⌃⌥Space brings the window forward from anywhere.
    func setupHotKey() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, ctx in
            let me = Unmanaged<Shell>.fromOpaque(ctx!).takeUnretainedValue()
            DispatchQueue.main.async { me.showWindow() }
            return noErr
        }, 1, &spec, Unmanaged.passUnretained(self).toOpaque(), nil)
        let id = EventHotKeyID(signature: OSType(0x4142_4F58), id: 1)
        RegisterEventHotKey(UInt32(kVK_Space), UInt32(controlKey | optionKey), id, GetApplicationEventTarget(), 0, &hotKey)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let shell = Shell()
app.delegate = shell
app.run()
