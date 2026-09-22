import Foundation
import CoreGraphics

enum Handle: CaseIterable {
    case move, left, right, top, bottom, topLeft, topRight, bottomLeft, bottomRight
}

/// Coordinates are display-local logical points, top-left origin. No global origin or
/// Retina scale enters the stored normalized ROI; XCap crops against physical pixels.
struct Selection {
    var rect: CGRect
    let bounds: CGRect
    init(normalized: CGRect, size: CGSize) {
        bounds = CGRect(origin: .zero, size: size)
        let n = normalized
        if [n.origin.x, n.origin.y, n.size.width, n.size.height].allSatisfy({ $0.isFinite }) &&
            n.origin.x >= 0 && n.origin.y >= 0 && n.size.width > 0 && n.size.height > 0 &&
            n.maxX <= 1.00001 && n.maxY <= 1.00001 {
            rect = CGRect(x: n.minX * size.width, y: n.minY * size.height,
                          width: n.width * size.width, height: n.height * size.height).intersection(bounds)
        } else {
            rect = bounds.insetBy(dx: size.width * 0.1, dy: size.height * 0.1)
        }
    }
    var normalized: CGRect {
        CGRect(x: rect.minX / bounds.width, y: rect.minY / bounds.height,
               width: rect.width / bounds.width, height: rect.height / bounds.height)
    }
    func hit(_ p: CGPoint) -> Handle? {
        let tolerance: CGFloat = 10
        guard rect.insetBy(dx: -tolerance, dy: -tolerance).contains(p) else { return nil }
        let l = abs(p.x - rect.minX) <= tolerance, r = abs(p.x - rect.maxX) <= tolerance
        let t = abs(p.y - rect.minY) <= tolerance, b = abs(p.y - rect.maxY) <= tolerance
        if l && t { return .topLeft }; if r && t { return .topRight }
        if l && b { return .bottomLeft }; if r && b { return .bottomRight }
        if l { return .left }; if r { return .right }; if t { return .top }; if b { return .bottom }
        return rect.contains(p) ? .move : nil
    }
    mutating func drag(_ handle: Handle, from original: CGRect, delta: CGSize) {
        let minW = min(32, original.width), minH = min(32, original.height)
        func clamp(_ v: CGFloat, _ lower: CGFloat, _ upper: CGFloat) -> CGFloat { min(max(v, lower), upper) }
        if handle == .move {
            rect = CGRect(x: clamp(original.minX + delta.width, 0, bounds.width - original.width),
                          y: clamp(original.minY + delta.height, 0, bounds.height - original.height),
                          width: original.width, height: original.height)
            return
        }
        var l = original.minX, r = original.maxX, t = original.minY, b = original.maxY
        if [.left, .topLeft, .bottomLeft].contains(handle) { l = clamp(l + delta.width, 0, r - minW) }
        if [.right, .topRight, .bottomRight].contains(handle) { r = clamp(r + delta.width, l + minW, bounds.width) }
        if [.top, .topLeft, .topRight].contains(handle) { t = clamp(t + delta.height, 0, b - minH) }
        if [.bottom, .bottomLeft, .bottomRight].contains(handle) { b = clamp(b + delta.height, t + minH, bounds.height) }
        rect = CGRect(x: l, y: t, width: r - l, height: b - t)
    }
}
