import AppKit
import CoreGraphics

final class OverlayWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown, let view = contentView as? OverlayView {
            if event.keyCode == 53 { view.cancel(); return }
            if event.keyCode == 36 || event.keyCode == 76 { view.confirm(); return }
        }
        super.sendEvent(event)
    }
}
final class OverlayView: NSView {
    var selection: Selection
    let preview: NSImage?
    let onFinish: (CGRect?) -> Void
    private var dragHandle: Handle?
    private var dragOrigin = CGPoint.zero
    private var original = CGRect.zero
    private var tracking: NSTrackingArea?
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    init(frame: CGRect, roi: CGRect, preview: NSImage?, onFinish: @escaping (CGRect?) -> Void) {
        selection = Selection(normalized: roi, size: frame.size)
        self.preview = preview
        self.onFinish = onFinish
        super.init(frame: frame)
        let controls = NSVisualEffectView(frame: CGRect(x: max(0, (frame.width - 480) / 2), y: 32, width: 480, height: 84))
        controls.material = .hudWindow
        controls.blendingMode = .withinWindow
        controls.state = .active
        controls.wantsLayer = true
        controls.layer?.cornerRadius = 12
        let label = NSTextField(labelWithString: "拖动四角或边缘缩放 · 拖动框内移动 · Enter 保存 · Esc 取消")
        label.frame = CGRect(x: 12, y: 53, width: 456, height: 20)
        label.font = .systemFont(ofSize: 12)
        label.alignment = .center
        controls.addSubview(label)
        for (title, action, x) in [("取消", #selector(cancel), 56.0), ("恢复全屏", #selector(reset), 190.0), ("保存范围", #selector(confirm), 324.0)] {
            let button = NSButton(title: title, target: self, action: action)
            button.bezelStyle = .rounded
            button.frame = CGRect(x: x, y: 13, width: 100, height: 30)
            controls.addSubview(button)
        }
        addSubview(controls)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let old = tracking { removeTrackingArea(old) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseMoved, .activeAlways, .inVisibleRect], owner: self)
        tracking = area
        addTrackingArea(area)
    }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.clear.setFill()
        if let preview { preview.draw(in: bounds, from: .zero, operation: .sourceOver, fraction: 1) }
        bounds.fill(using: .copy)
        // A tiny alpha keeps the transparent interior mouse-interactive on macOS.
        NSColor.white.withAlphaComponent(0.01).setFill()
        bounds.fill()
        let r = selection.rect
        let mask = NSBezierPath(rect: bounds)
        mask.append(NSBezierPath(rect: r))
        mask.windingRule = .evenOdd
        NSColor.black.withAlphaComponent(0.40).setFill()
        mask.fill()
        NSColor.white.withAlphaComponent(0.7).setStroke()
        let border = NSBezierPath(rect: r.insetBy(dx: 0.75, dy: 0.75))
        border.lineWidth = 1.5
        border.stroke()
        let accent = NSColor(calibratedRed: 0.68, green: 0.85, blue: 0.93, alpha: 1)
        accent.setStroke()
        // Four L-shaped corners remain visible even when the selection is full-screen.
        let length: CGFloat = min(22, min(r.width, r.height) / 3)
        for (p, dx, dy) in [(CGPoint(x: r.minX+2,y: r.minY+2),1.0,1.0),
                             (CGPoint(x: r.maxX-2,y: r.minY+2),-1.0,1.0),
                             (CGPoint(x: r.minX+2,y: r.maxY-2),1.0,-1.0),
                             (CGPoint(x: r.maxX-2,y: r.maxY-2),-1.0,-1.0)] {
            let corner = NSBezierPath()
            corner.move(to: CGPoint(x: p.x + dx * length, y: p.y))
            corner.line(to: p)
            corner.line(to: CGPoint(x: p.x, y: p.y + dy * length))
            corner.lineWidth = 3
            corner.stroke()
        }
        let center = CGPoint(x: r.midX, y: r.midY)
        let cross = NSBezierPath()
        cross.move(to: CGPoint(x: center.x-12,y: center.y)); cross.line(to: CGPoint(x: center.x+12,y: center.y))
        cross.move(to: CGPoint(x: center.x,y: center.y-12)); cross.line(to: CGPoint(x: center.x,y: center.y+12))
        cross.lineWidth = 2
        cross.stroke()
        let scale = window?.backingScaleFactor ?? 1
        let text = "\(Int((r.width * scale).rounded())) × \(Int((r.height * scale).rounded())) px"
        let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedDigitSystemFont(ofSize: 13, weight: .medium), .foregroundColor: NSColor.white, .backgroundColor: NSColor.black.withAlphaComponent(0.65)]
        (text as NSString).draw(at: CGPoint(x: max(4, r.midX-65), y: min(bounds.height-24, r.midY+18)), withAttributes: attrs)
    }
    private func cursor(_ handle: Handle?) {
        switch handle {
        case .left, .right: NSCursor.resizeLeftRight.set()
        case .top, .bottom: NSCursor.resizeUpDown.set()
        case .topLeft, .bottomRight, .topRight, .bottomLeft: NSCursor.crosshair.set()
        case .move: NSCursor.openHand.set()
        case nil: NSCursor.arrow.set()
        }
    }
    override func mouseMoved(with event: NSEvent) { cursor(selection.hit(convert(event.locationInWindow, from: nil))) }
    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        dragOrigin = convert(event.locationInWindow, from: nil)
        dragHandle = selection.hit(dragOrigin)
        original = selection.rect
        if dragHandle == .move { NSCursor.closedHand.set() }
    }
    override func mouseDragged(with event: NSEvent) {
        guard let handle = dragHandle else { return }
        let p = convert(event.locationInWindow, from: nil)
        selection.drag(handle, from: original, delta: CGSize(width: p.x - dragOrigin.x, height: p.y - dragOrigin.y))
        needsDisplay = true
    }
    override func mouseUp(with event: NSEvent) {
        mouseDragged(with: event)
        dragHandle = nil
        cursor(selection.hit(convert(event.locationInWindow, from: nil)))
    }
    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76: confirm()
        case 53: cancel()
        default: super.keyDown(with: event)
        }
    }
    @objc func confirm() { onFinish(selection.normalized) }
    @objc func cancel() { onFinish(nil) }
    @objc func reset() { selection.rect = bounds; needsDisplay = true }
}
