import AppKit
import WebKit

// The helper is owned by one Airs browser session. All WebKit and AppKit
// methods execute on the main thread; the parent communicates through pipes.
final class Host: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler, NSWindowDelegate {
    typealias Reply = (Any?, String?) -> Void
    private var view: WKWebView!
    private let window: NSWindow
    private var navigationReply: Reply?
    private var pendingFiles: ([URL], Reply, Int)?
    private var frameSequence = 0
    private var fileSequence = 0
    private var receiptToken: String?
    private var receiptEndpoints = Set<String>()
    private var receiptDocumentOrigin: String?
    private var receipts = [[String: Any]]()
    private var frames = [String: WKFrameInfo]()
    private var pendingFrame: (token: String, origin: String, source: String, reply: Reply)?

    init(profile: UUID, persistent: Bool) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = persistent ? WKWebsiteDataStore(forIdentifier: profile) : .nonPersistent()
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1440, height: 1000), configuration: config)
        window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        super.init()
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.contentView = view
        view.navigationDelegate = self
        view.uiDelegate = self
        view.configuration.userContentController.add(self, name: "airsReceipt")
        view.configuration.userContentController.add(self, name: "airsFrame")
        view.configuration.userContentController.addUserScript(WKUserScript(source: receiptMonitor,
            injectionTime: .atDocumentStart, forMainFrameOnly: false))
    }

    func run() {
        DispatchQueue.global(qos: .userInitiated).async {
            while let line = readLine() {
                guard let data = line.data(using: .utf8),
                      let request = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let id = request["id"] as? Int else { continue }
                DispatchQueue.main.async {
                    self.dispatch(request) { value, error in
                        var response: [String: Any] = ["id": id]
                        if let error = error { response["error"] = error }
                        else { response["result"] = value ?? NSNull() }
                        guard let bytes = try? JSONSerialization.data(withJSONObject: response),
                              let json = String(data: bytes, encoding: .utf8) else {
                            print("{\"id\":\(id),\"error\":\"Invalid response\"}")
                            fflush(stdout)
                            return
                        }
                        print(json)
                        fflush(stdout)
                    }
                }
            }
            DispatchQueue.main.async { NSApplication.shared.stop(nil); exit(0) }
        }
    }

    private func dispatch(_ r: [String: Any], reply: @escaping Reply) {
        guard let action = r["action"] as? String else { reply(nil, "Missing action"); return }
        switch action {
        case "navigate":
            guard let text = r["url"] as? String, let url = URL(string: text),
                  ["http", "https", "about"].contains(url.scheme ?? "") else {
                reply(nil, "Invalid navigation"); return
            }
            beginNavigation(reply)
            view.load(URLRequest(url: url))
        case "reload":
            beginNavigation(reply)
            view.reload()
        case "eval":
            guard let source = r["source"] as? String else { reply(nil, "Missing script"); return }
            if let frameURL = r["frameURL"] as? String {
                guard let frame = frames[frameURL] else { reply(nil, "Frame unavailable"); return }
                evaluate(source, frame: frame, reply: reply)
            } else {
                evaluate(source, reply: reply)
            }
        case "evalFrameAt":
            guard let x = r["x"] as? Double, let y = r["y"] as? Double,
                  let origin = r["origin"] as? String, let script = r["source"] as? String,
                  validPoint(x, y), pendingFrame == nil else { reply(nil, "Invalid frame request"); return }
            let token = UUID().uuidString
            pendingFrame = (token, origin, script, reply)
            view.callAsyncJavaScript("""
                const frame = document.elementFromPoint(x, y);
                if (frame?.localName !== 'iframe' || new URL(frame.src).origin !== origin) return false;
                frame.contentWindow.postMessage({__airsFrameProbe:token}, origin);
                return true;
                """, arguments: ["x":x, "y":y, "origin":origin, "token":token], in: nil, in: .page) { result in
                    if case .success(let value) = result, (value as? Bool) == true { return }
                    self.finishFrame(token, "Frame unavailable")
                }
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                self.finishFrame(token, "Frame resolution timed out")
            }
        case "armDouyinWorkList":
            guard view.url == nil || view.url?.absoluteString == "about:blank",
                  let title = r["title"] as? String, !title.isEmpty, title.count <= 256,
                  let description = r["description"] as? String, description.count <= 4096,
                  let since = r["since"] as? Int, let until = r["until"] as? Int,
                  since > 0, until >= since, until - since <= 900,
                  let data = try? JSONSerialization.data(withJSONObject:
                    ["title": title, "description": description, "since": since, "until": until]),
                  let parameters = String(data: data, encoding: .utf8) else {
                reply(nil, "Invalid read-only work lookup"); return
            }
            let source = "(\(workListMonitor))(\(parameters),(\(matchDouyinWorks)))"
            view.configuration.userContentController.addUserScript(WKUserScript(
                source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true))
            reply(true, nil)
        case "armReceipts":
            guard let endpoints = r["endpoints"] as? [String], !endpoints.isEmpty, endpoints.count <= 12,
                  let doc = view.url, let scheme = doc.scheme, let host = doc.host else {
                reply(nil, "Invalid receipt filter"); return
            }
            let origin = scheme + "://" + host + (doc.port.map { ":\($0)" } ?? "")
            let token = UUID().uuidString
            receiptToken = token
            receiptEndpoints = Set(endpoints)
            receiptDocumentOrigin = origin
            receipts.removeAll()
            let sameOriginFrames = frames.values.filter {
                guard !$0.isMainFrame, let url = $0.request.url else { return false }
                return url.scheme == doc.scheme && url.host == doc.host && url.port == doc.port
            }
            let targets: [WKFrameInfo?] = [nil] + sameOriginFrames.map { Optional($0) }
            let group = DispatchGroup()
            var mainArmed = false
            for target in targets {
                group.enter()
                view.callAsyncJavaScript("return window.__airsArmReceipts(token, endpoints);",
                    arguments: ["token": token, "endpoints": endpoints], in: target, in: .page) { result in
                    if target == nil, case .success(let value) = result, (value as? Bool) == true {
                        mainArmed = true
                    }
                    group.leave()
                }
            }
            group.notify(queue: .main) { reply(mainArmed, mainArmed ? nil : "Receipt monitor unavailable") }
        case "takeReceipts":
            let result = receipts
            receipts.removeAll()
            reply(result, nil)
        case "selectFiles":
            guard let files = r["paths"] as? [String], !files.isEmpty,
                  let expression = r["element"] as? String, pendingFiles == nil,
                  files.allSatisfy({ path in
                      var isDirectory: ObjCBool = false
                      return path.hasPrefix("/") && FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) && !isDirectory.boolValue
                  }) else { reply(nil, "Invalid file selection"); return }
            let urls = files.map { URL(fileURLWithPath: $0) }
            fileSequence += 1
            let token = fileSequence
            pendingFiles = (urls, reply, token)
            let clickPoint = r["clickPoint"] as? [Double]
            if let point = clickPoint, point.count != 2 || !validPoint(point[0], point[1]) {
                completeFiles(false, "Invalid file upload position")
                return
            }
            evaluate("""
                (() => {
                  const input = (\(expression));
                  if (!input || input.localName !== 'input' || input.type !== 'file' ||
                      !input.isConnected || input.disabled || input.matches(':disabled') ||
                      (\(files.count) > 1 && !input.multiple)) return false;
                  globalThis.__airsFileGuard?.cleanup();
                  let matched = false;
                  let mismatch = false;
                  const guard = (event) => {
                    const candidate = event.composedPath()[0];
                    if (candidate?.localName === 'input' && candidate.type === 'file') {
                      if (candidate === input) matched = true;
                      else {
                        mismatch = true;
                        event.preventDefault();
                        event.stopImmediatePropagation();
                      }
                    }
                  };
                  const doc = input.ownerDocument;
                  const root = doc.defaultView.Node.prototype.getRootNode.call(input);
                  doc.addEventListener('click', guard, true);
                  if (root !== doc) root.addEventListener('click', guard, true);
                  globalThis.__airsFileGuard = {
                    token: \(token),
                    matched: () => matched && !mismatch && input.isConnected,
                    mismatch: () => mismatch,
                    cleanup: () => {
                      doc.removeEventListener('click', guard, true);
                      if (root !== doc) root.removeEventListener('click', guard, true);
                    }
                  };
                  if (\(clickPoint == nil)) input.click();
                  return true;
                })()
                """) { result, error in
                    if error != nil || (result as? Bool) != true {
                        self.completeFiles(false, error ?? "File input unavailable")
                    } else if let point = clickPoint, !self.sendMouse(x: point[0], y: point[1], click: true) {
                        self.completeFiles(false, "Mouse event failed")
                    } else if clickPoint != nil {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                            guard self.pendingFiles?.2 == token else { return }
                            self.evaluate("globalThis.__airsFileGuard?.mismatch() === true") { value, _ in
                                if self.pendingFiles?.2 == token && (value as? Bool) == true {
                                    self.completeFiles(false, "Unexpected file input")
                                }
                            }
                        }
                    }
                }
            DispatchQueue.main.asyncAfter(deadline: .now() + 8) {
                if self.fileSequence == token { self.completeFiles(false, "File chooser timed out") }
            }
        case "click", "moveMouse":
            guard let x = r["x"] as? Double, let y = r["y"] as? Double,
                  validPoint(x, y),
                  sendMouse(x: x, y: y, click: action == "click") else {
                reply(nil, "Invalid mouse position"); return
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.10) {
                self.evaluate("true", reply: reply)
            }
        case "key":
            guard let key = r["key"] as? String,
                  let code: UInt16 = ["Enter": 36, "Backspace": 51, "Escape": 53][key],
                  let chars = ["Enter": "\r", "Backspace": "\u{8}", "Escape": "\u{1b}"][key] else {
                reply(nil, "Unsupported key"); return
            }
            let timestamp = ProcessInfo.processInfo.systemUptime
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                guard let event = NSEvent.keyEvent(with: type, location: .zero,
                    modifierFlags: [], timestamp: timestamp, windowNumber: window.windowNumber,
                    context: nil, characters: chars, charactersIgnoringModifiers: chars,
                    isARepeat: false, keyCode: code) else {
                    reply(nil, "Key event failed"); return
                }
                if type == .keyDown { view.keyDown(with: event) }
                else { view.keyUp(with: event) }
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.10) {
                self.evaluate("true", reply: reply)
            }
        case "showDouyinLogin":
            guard let url = view.url, url.scheme == "https", url.host == "creator.douyin.com" else {
                reply(nil, "Official login page is unavailable"); return
            }
            if !window.isVisible {
                NSApplication.shared.setActivationPolicy(.accessory)
                window.styleMask = [.titled, .closable]
                window.title = "Airs · 抖音登录验证"
                if let bounds = NSScreen.main?.visibleFrame {
                    window.setContentSize(NSSize(width: min(1440, bounds.width - 40),
                        height: min(1000, bounds.height - 60)))
                }
                window.center()
            }
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(view)
            NSApplication.shared.activate()
            reply(true, nil)
        case "hideDouyinLogin":
            hideLoginWindow()
            reply(true, nil)
        case "snapshot":
            let config = WKSnapshotConfiguration()
            if let clip = r["clip"] as? [String: Double],
               let x = clip["x"], let y = clip["y"], let width = clip["width"], let height = clip["height"] {
                guard x.isFinite, y.isFinite, width.isFinite, height.isFinite,
                      width > 0, height > 0, x >= 0, y >= 0,
                      x + width <= view.bounds.width, y + height <= view.bounds.height else {
                    reply(nil, "Invalid snapshot bounds"); return
                }
                config.rect = CGRect(x: x, y: y, width: width, height: height)
            }
            view.takeSnapshot(with: config) { image, error in
                guard let image = image, let tiff = image.tiffRepresentation,
                      let bitmap = NSBitmapImageRep(data: tiff),
                      let png = bitmap.representation(using: .png, properties: [:]) else {
                    reply(nil, error?.localizedDescription ?? "Snapshot failed"); return
                }
                reply(png.base64EncodedString(), nil)
            }
        case "close":
            completeFiles(false, "Session closed")
            view.stopLoading()
            view.configuration.userContentController.removeScriptMessageHandler(forName: "airsReceipt")
            view.configuration.userContentController.removeScriptMessageHandler(forName: "airsFrame")
            view.navigationDelegate = nil
            view.uiDelegate = nil
            window.contentView = nil
            view = nil
            window.close()
            reply(true, nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { exit(0) }
        default:
            reply(nil, "Unsupported browser operation")
        }
    }

    private func hideLoginWindow() {
        guard window.isVisible else { return }
        window.orderOut(nil)
        window.styleMask = [.borderless]
        window.setContentSize(NSSize(width: 1440, height: 1000))
        NSApplication.shared.setActivationPolicy(.prohibited)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        // Closing the verification window never destroys the active session.
        hideLoginWindow()
        return false
    }

    private func finishFrame(_ token: String, _ error: String) {
        if let pending = pendingFrame, pending.token == token {
            pendingFrame = nil
            pending.reply(nil, error)
        }
    }

    private func validPoint(_ x: Double, _ y: Double) -> Bool {
        x.isFinite && y.isFinite && x >= 0 && y >= 0 &&
        x < view.bounds.width && y < view.bounds.height
    }

    private func sendMouse(x: Double, y: Double, click: Bool) -> Bool {
        guard validPoint(x, y) else { return false }
        let point = NSPoint(x: x, y: view.bounds.height - y)
        let time = ProcessInfo.processInfo.systemUptime
        func event(_ type: NSEvent.EventType, pressure: Float) -> NSEvent? {
            NSEvent.mouseEvent(with: type, location: point, modifierFlags: [],
                timestamp: time, windowNumber: window.windowNumber, context: nil,
                eventNumber: 1, clickCount: 1, pressure: pressure)
        }
        if let move = event(.mouseMoved, pressure: 0) { view.mouseMoved(with: move) }
        if click {
            guard let down = event(.leftMouseDown, pressure: 1),
                  let up = event(.leftMouseUp, pressure: 0) else { return false }
            view.mouseDown(with: down)
            view.mouseUp(with: up)
        }
        return true
    }

    private func beginNavigation(_ reply: @escaping Reply) {
        navigationReply?(nil, "Navigation replaced")
        navigationReply = reply
        frames.removeAll()
        frameSequence += 1
        let token = frameSequence
        DispatchQueue.main.asyncAfter(deadline: .now() + 25) {
            if self.frameSequence == token { self.finishNavigation("Navigation timed out") }
        }
    }

    private func finishNavigation(_ error: String?) {
        let callback = navigationReply
        navigationReply = nil
        callback?(true, error)
    }

    private func evaluate(_ source: String, frame: WKFrameInfo? = nil, reply: @escaping Reply) {
        view.callAsyncJavaScript("return await (0, eval)(source);", arguments: ["source": source],
            in: frame, in: .page) { result in
            switch result {
            case .success(let value): reply(value, nil)
            case .failure(let error): reply(nil, error.localizedDescription)
            }
        }
    }

    private func completeFiles(_ accepted: Bool, _ error: String?) {
        let pending = pendingFiles
        pendingFiles = nil
        if let pending = pending {
            evaluate("globalThis.__airsFileGuard?.cleanup(); null") { _,_ in }
            pending.1(accepted, error)
        }
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        if message.name == "airsFrame" {
            if let data = message.body as? [String:String], let token = data["token"],
               let pending = pendingFrame, pending.token == token, !message.frameInfo.isMainFrame,
               let frame = message.frameInfo.request.url, let scheme = frame.scheme, let host = frame.host {
                let origin = scheme + "://" + host + (frame.port.map { ":\($0)" } ?? "")
                if origin == pending.origin {
                    pendingFrame = nil
                    evaluate(pending.source, frame: message.frameInfo, reply: pending.reply)
                }
                return
            }
            if let url = message.body as? String, let candidate = URL(string: url),
               let frame = message.frameInfo.request.url,
               candidate.scheme == frame.scheme, candidate.host == frame.host, candidate.port == frame.port {
                frames[url] = message.frameInfo
            }
            return
        }
        guard message.name == "airsReceipt",
              let body = message.body as? [String: Any],
              let token = body["token"] as? String, token == receiptToken,
              let endpoint = body["key"] as? String, receiptEndpoints.contains(endpoint),
              let document = body["document"] as? String,
              let page = URL(string: document), let host = page.host, let scheme = page.scheme,
              let frame = message.frameInfo.request.url, frame.host == host, frame.scheme == scheme,
              let status = body["status"] as? Int, (200..<300).contains(status),
              let response = body["body"] as? String, response.utf8.count <= 262144,
              receipts.count < 16 else { return }
        let origin = scheme + "://" + host + (page.port.map { ":\($0)" } ?? "")
        guard origin == receiptDocumentOrigin else { return }
        let request = body["request"] as? String
        guard request == nil || request!.utf8.count <= 262144 else { return }
        receipts.append(["endpoint": endpoint, "document": document, "status": status,
                         "request": request as Any? ?? NSNull(), "body": response])
    }

    // A webpage is not permission to launch an unrelated macOS application.
    // Intercept external navigation before WebKit hands it to Launch Services.
    // Preserve ordinary web navigation, including in-page blobs and data URLs.
    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url,
              let scheme = url.scheme?.lowercased() else {
            decisionHandler(.cancel)
            return
        }
        if ["http", "https", "about", "blob", "data"].contains(scheme) {
            decisionHandler(.allow)
            return
        }

        // The existing video-channel desktop login can intentionally use the
        // installed WeChat app. It is the only supported external-app handoff.
        let source = navigationAction.sourceFrame.securityOrigin
        if ["weixin", "wechat"].contains(scheme),
           source.protocol == "https",
           ["open.weixin.qq.com", "channels.weixin.qq.com"].contains(source.host),
           navigationAction.navigationType == .linkActivated,
           NSWorkspace.shared.urlForApplication(toOpen: url) != nil {
            decisionHandler(.cancel)
            NSWorkspace.shared.open(url)
            return
        }
        decisionHandler(.cancel)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        finishNavigation(nil)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { finishNavigation(error.localizedDescription) }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { finishNavigation(error.localizedDescription) }
    }

    func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping ([URL]?) -> Void) {
        guard let pending = pendingFiles,
              let source = view.url, let destination = frame.request.url,
              source.scheme == destination.scheme, source.host == destination.host, source.port == destination.port,
              pending.0.count == 1 || parameters.allowsMultipleSelection else {
            completionHandler(nil)
            completeFiles(false, "Unexpected file chooser")
            return
        }
        evaluate("globalThis.__airsFileGuard?.token === \(pending.2) && globalThis.__airsFileGuard.matched()") { value, error in
            guard self.pendingFiles?.2 == pending.2 else { completionHandler(nil); return }
            let matched = error == nil && (value as? Bool) == true
            completionHandler(matched ? pending.0 : nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.10) {
                guard self.pendingFiles?.2 == pending.2 else { return }
                self.completeFiles(matched, matched ? nil : "Unexpected file input")
            }
        }
    }
}

guard CommandLine.arguments.count == 3,
      let profile = UUID(uuidString: CommandLine.arguments[1]) else {
    fputs("Invalid WebKit profile.\n", stderr)
    exit(1)
}
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
if CommandLine.arguments[2] == "remove" {
    // Initialize WebKit's process run loop before invoking the asynchronous
    // profile-removal API in this short-lived helper.
    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = .nonPersistent()
    let initializer = WKWebView(frame: .zero, configuration: configuration)
    DispatchQueue.main.async {
        _ = initializer
        WKWebsiteDataStore.remove(forIdentifier: profile) { error in
            if let error = error { fputs(error.localizedDescription + "\n", stderr); exit(1) }
            exit(0)
        }
    }
    RunLoop.main.run()
    exit(1)
}
let host = Host(profile: profile, persistent: CommandLine.arguments[2] == "persistent")
host.run()
app.run()
