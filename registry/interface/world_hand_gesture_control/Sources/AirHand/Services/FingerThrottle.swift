import Foundation

enum FingerThrottle {
    static let minFactor = 0.12
    static let curlFullSpeed = 0.12
    static let curlMinSpeed = 0.40

    static func speedFactor(indexCurl: Double) -> Double {
        let t = min(max((indexCurl - curlFullSpeed) / (curlMinSpeed - curlFullSpeed), 0), 1)
        return 1 - (1 - minFactor) * t
    }
}
