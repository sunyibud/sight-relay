import AppKit

@main struct InteractionTests {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let window = OverlayWindow(contentRect: CGRect(x: 0, y: 0, width: 960, height: 600), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isOpaque = false
        window.backgroundColor = .clear
        let initial = CGRect(x: 0.2, y: 0.25, width: 0.6, height: 0.5)
        var outcomes: [CGRect?] = []
        let view = OverlayView(frame: CGRect(x: 0,y: 0,width: 960,height: 600), roi: initial, preview: nil) { outcomes.append($0) }
        window.contentView = view
        func mouse(_ type: NSEvent.EventType, _ p: CGPoint) -> NSEvent {
            NSEvent.mouseEvent(with: type, location: view.convert(p, to: nil), modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        }
        func key(_ code: UInt16) -> NSEvent {
            NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: "", charactersIgnoringModifiers: "", isARepeat: false, keyCode: code)!
        }
        let r = view.selection.rect
        view.mouseDown(with: mouse(.leftMouseDown, CGPoint(x: r.maxX,y: r.maxY)))
        view.mouseDragged(with: mouse(.leftMouseDragged, CGPoint(x: r.maxX+96,y: r.maxY+60)))
        view.mouseUp(with: mouse(.leftMouseUp, CGPoint(x: r.maxX+96,y: r.maxY+60)))
        assert(view.selection.rect.width == r.width+96 && view.selection.rect.height == r.height+60)
        window.sendEvent(key(36))
        assert(outcomes.count == 1 && outcomes[0]! == view.selection.normalized)
        print("PASS native mouse corner resize and Enter result")
        view.reset()
        assert(view.selection.normalized == CGRect(x: 0,y: 0,width: 1,height: 1))
        window.sendEvent(key(53))
        assert(outcomes.count == 2 && outcomes[1] == nil)
        print("PASS reset and Escape cancellation")
        view.selection = Selection(normalized: initial, size: view.bounds.size)
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { fatalError("no bitmap") }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        let sx = CGFloat(bitmap.pixelsWide) / view.bounds.width
        let sy = CGFloat(bitmap.pixelsHigh) / view.bounds.height
        let inside = bitmap.colorAt(x: Int(300*sx), y: Int(250*sy))!.alphaComponent
        let outside = bitmap.colorAt(x: Int(20*sx), y: Int(300*sy))!.alphaComponent
        assert(inside < 0.03 && outside > 0.35 && outside < 0.45, "Unexpected transparency: \(inside), \(outside)")
        print("PASS transparent interior and dimmed exterior")
        let png = bitmap.representation(using: .png, properties: [:])!
        try! png.write(to: URL(fileURLWithPath: "/tmp/sight-relay-roi-overlay.png"))
        print("PASS native overlay render → /tmp/sight-relay-roi-overlay.png")
    }
}
