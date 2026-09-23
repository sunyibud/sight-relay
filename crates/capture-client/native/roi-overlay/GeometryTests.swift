import Foundation
import CoreGraphics

@main struct GeometryTests {
    static func main() {
        var failures = 0
        func check(_ ok: Bool, _ name: String) {
            print("\(ok ? "PASS" : "FAIL") \(name)")
            if !ok { failures += 1 }
        }
        let size = CGSize(width: 1440, height: 900)
        let input = CGRect(x: 0.25, y: 0.2, width: 0.5, height: 0.6)
        var s = Selection(normalized: input, size: size)
        check(s.rect == CGRect(x: 360, y: 180, width: 720, height: 540), "normalized ROI uses screen points")
        check(s.normalized == input, "Retina independent round trip")
        let initial = s.rect
        let points: [(CGPoint, Handle)] = [
            (CGPoint(x: 360, y: 180), .topLeft), (CGPoint(x: 1080, y: 180), .topRight),
            (CGPoint(x: 360, y: 720), .bottomLeft), (CGPoint(x: 1080, y: 720), .bottomRight),
            (CGPoint(x: 360, y: 450), .left), (CGPoint(x: 1080, y: 450), .right),
            (CGPoint(x: 720, y: 180), .top), (CGPoint(x: 720, y: 720), .bottom),
            (CGPoint(x: 720, y: 450), .move)
        ]
        for (p, h) in points { check(s.hit(p) == h, "hit \(h)") }
        check(s.hit(CGPoint(x: 20, y: 20)) == nil, "outside does not create new selection")
        s.drag(.move, from: initial, delta: CGSize(width: 2000, height: -2000))
        check(s.rect == CGRect(x: 720, y: 0, width: 720, height: 540), "moving clamps without resizing")
        s.drag(.left, from: initial, delta: CGSize(width: 100, height: 500))
        check(s.rect == CGRect(x: 460, y: 180, width: 620, height: 540), "edge drag changes only its axis")
        s.drag(.topLeft, from: initial, delta: CGSize(width: 2000, height: 2000))
        check(s.rect.width == 32 && s.rect.height == 32 && s.rect.maxX == initial.maxX && s.rect.maxY == initial.maxY, "cannot invert rectangle or shrink below minimum")
        s.drag(.bottomRight, from: initial, delta: CGSize(width: 2000, height: 2000))
        check(s.rect.maxX == 1440 && s.rect.maxY == 900, "resizing clamps to screen")
        let invalid = Selection(normalized: CGRect(x: -1, y: 0, width: 0, height: 1), size: size)
        check(invalid.rect.width > 0 && invalid.rect.minX >= 0, "invalid saved range has valid fallback")
        exit(failures == 0 ? 0 : 1)
    }
}
