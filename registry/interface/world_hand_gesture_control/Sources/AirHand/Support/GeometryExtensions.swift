import CoreGraphics

extension CGPoint {
    static func + (lhs: CGPoint, rhs: CGPoint) -> CGPoint {
        CGPoint(x: lhs.x + rhs.x, y: lhs.y + rhs.y)
    }

    static func - (lhs: CGPoint, rhs: CGPoint) -> CGPoint {
        CGPoint(x: lhs.x - rhs.x, y: lhs.y - rhs.y)
    }

    static func * (lhs: CGPoint, rhs: CGFloat) -> CGPoint {
        CGPoint(x: lhs.x * rhs, y: lhs.y * rhs)
    }

    static func / (lhs: CGPoint, rhs: CGFloat) -> CGPoint {
        CGPoint(x: lhs.x / rhs, y: lhs.y / rhs)
    }

    func dot(_ other: CGPoint) -> CGFloat {
        x * other.x + y * other.y
    }

    var length: CGFloat {
        sqrt(x * x + y * y)
    }

    func distance(to other: CGPoint) -> CGFloat {
        (self - other).length
    }

    func distance(to rect: CGRect) -> CGFloat {
        let dx = max(rect.minX - x, 0, x - rect.maxX)
        let dy = max(rect.minY - y, 0, y - rect.maxY)
        return sqrt(dx * dx + dy * dy)
    }

    func normalized() -> CGPoint {
        let len = length
        guard len > 0.000_001 else { return .zero }
        return self / len
    }

    func clamped(to rect: CGRect) -> CGPoint {
        CGPoint(
            x: min(max(x, rect.minX), rect.maxX),
            y: min(max(y, rect.minY), rect.maxY)
        )
    }

    var roundedToPixel: CGPoint {
        CGPoint(x: x.rounded(), y: y.rounded())
    }
}

extension CGRect {
    static func union(_ rects: [CGRect]) -> CGRect {
        guard let first = rects.first else { return .zero }
        return rects.dropFirst().reduce(first) { $0.union($1) }
    }
}
