import AppKit
import CoreGraphics

@main struct RoiOverlay {
    static func main() {
        let args = Array(CommandLine.arguments.dropFirst())
        guard args.count == 6, let display = UInt32(args[0]),
              let x = Double(args[2]), let y = Double(args[3]), let w = Double(args[4]), let h = Double(args[5]) else {
            fputs("Usage: roi-overlay display_id preview_path x y width height\n", stderr); exit(2)
        }
        let previewPath = args[1]
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        guard let screen = NSScreen.screens.first(where: { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == display }) else {
            fputs("Selected display is no longer connected\n", stderr); exit(2)
        }
        let window = OverlayWindow(contentRect: screen.frame, styleMask: [.borderless], backing: .buffered, defer: false, screen: screen)
        window.setFrame(screen.frame, display: false)
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = false
        window.level = .screenSaver
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.acceptsMouseMovedEvents = true
        window.isReleasedWhenClosed = false
        let view = OverlayView(frame: CGRect(origin: .zero, size: screen.frame.size), roi: CGRect(x: x,y: y,width: w,height: h), preview: NSImage(contentsOfFile: previewPath)) { roi in
            window.orderOut(nil)
            if let r = roi, let data = try? JSONSerialization.data(withJSONObject: ["x":r.minX,"y":r.minY,"width":r.width,"height":r.height]) {
                FileHandle.standardOutput.write(data)
                FileHandle.standardOutput.write(Data("\n".utf8))
            }
            app.stop(nil)
        }
        window.contentView = view
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(view)
        app.activate(ignoringOtherApps: true)
        // Display topology changes invalidate the current coordinate frame: cancel instead of saving a wrong ROI.
        let observer = NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { _ in view.cancel() }
        app.run()
        NotificationCenter.default.removeObserver(observer)
    }
}
