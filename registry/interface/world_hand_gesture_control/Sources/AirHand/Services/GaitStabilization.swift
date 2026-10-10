import CoreGraphics
import Foundation

struct GaitProfile: Codable, Equatable {
    var fundamentalHz: Double
    var harmonics: [Double]
    var amplitude: Double
    var confidence: Double
    var sampleRate: Double

    var isUsable: Bool {
        confidence >= 0.28 && fundamentalHz > 0.5 && fundamentalHz < 3.5 && !harmonics.isEmpty
    }

    func encodedString() throws -> String {
        let data = try JSONEncoder().encode(self)
        return String(decoding: data, as: UTF8.self)
    }

    init(fundamentalHz: Double, harmonics: [Double], amplitude: Double, confidence: Double, sampleRate: Double) {
        self.fundamentalHz = fundamentalHz
        self.harmonics = harmonics
        self.amplitude = amplitude
        self.confidence = confidence
        self.sampleRate = sampleRate
    }

    init?(encodedString: String) {
        guard !encodedString.isEmpty,
              let data = encodedString.data(using: .utf8),
              let decoded = try? JSONDecoder().decode(GaitProfile.self, from: data)
        else {
            return nil
        }
        self = decoded
    }
}

enum GaitAnalyzer {
    static func resample(points: [CGPoint], times: [TimeInterval], targetRate: Double) -> [CGPoint] {
        guard points.count == times.count, points.count >= 2, targetRate > 0,
              let start = times.first, let end = times.last, end - start > 1
        else {
            return points
        }
        let count = Int((end - start) * targetRate)
        guard count >= 2 else { return points }

        var output = [CGPoint]()
        output.reserveCapacity(count)
        var cursor = 0
        for index in 0..<count {
            let t = start + Double(index) / targetRate
            while cursor < times.count - 2, times[cursor + 1] < t { cursor += 1 }
            let ta = times[cursor]
            let tb = times[cursor + 1]
            let fraction = tb > ta ? CGFloat((t - ta) / (tb - ta)) : 0
            let a = points[cursor]
            let b = points[cursor + 1]
            output.append(CGPoint(x: a.x + (b.x - a.x) * fraction, y: a.y + (b.y - a.y) * fraction))
        }
        return output
    }

    static func analyze(samples: [CGPoint], sampleRate: Double) -> GaitProfile? {
        guard sampleRate > 1, samples.count >= Int(sampleRate * 5) else { return nil }

        let slowWindow = Int(sampleRate / 0.45)
        let fastWindow = max(Int(sampleRate / 5.0), 3)
        let x = smooth(detrend(samples.map { Double($0.x) }, window: slowWindow), window: fastWindow)
        let y = smooth(detrend(samples.map { Double($0.y) }, window: slowWindow), window: fastWindow)

        let minLag = max(Int(sampleRate / 3.0), 2)
        let maxLag = Int(sampleRate / 0.6)
        guard maxLag > minLag, samples.count > maxLag * 3 + 2 else { return nil }

        let acfX = autocorrelation(x, maxLag: maxLag * 3)
        let acfY = autocorrelation(y, maxLag: maxLag * 3)
        let energy = max(acfX[0] + acfY[0], 1e-9)
        let acf = (0...maxLag * 3).map { (acfX[$0] + acfY[$0]) / energy }

        var bestLag = 0
        var bestScore = -Double.greatestFiniteMagnitude
        for lag in minLag...maxLag {
            let score = acf[lag]
                + 0.7 * acf[min(lag * 2, acf.count - 1)]
                + 0.45 * acf[min(lag * 3, acf.count - 1)]
            if score > bestScore {
                bestScore = score
                bestLag = lag
            }
        }
        guard bestLag > 0 else { return nil }

        let fundamentalHz = sampleRate / Double(bestLag)
        let confidence = min(max(acf[bestLag], 0), 1)
        let amplitude = sqrt(variance(x) + variance(y))

        var harmonics: [Double] = []
        for multiple in 1...4 {
            let frequency = fundamentalHz * Double(multiple)
            if frequency < sampleRate * 0.45 {
                harmonics.append(frequency)
            }
        }

        let profile = GaitProfile(
            fundamentalHz: fundamentalHz,
            harmonics: harmonics,
            amplitude: amplitude,
            confidence: confidence,
            sampleRate: sampleRate
        )
        return profile.isUsable ? profile : nil
    }

    private static func detrend(_ values: [Double], window: Int) -> [Double] {
        guard values.count > window, window > 1 else {
            let mean = values.reduce(0, +) / Double(max(values.count, 1))
            return values.map { $0 - mean }
        }
        var prefix = [0.0]
        prefix.reserveCapacity(values.count + 1)
        for value in values { prefix.append(prefix[prefix.count - 1] + value) }

        let half = window / 2
        return values.indices.map { index in
            let low = max(0, index - half)
            let high = min(values.count, index + half + 1)
            let movingAverage = (prefix[high] - prefix[low]) / Double(high - low)
            return values[index] - movingAverage
        }
    }

    private static func smooth(_ values: [Double], window: Int) -> [Double] {
        guard window > 1, values.count > window else { return values }
        var prefix = [0.0]
        prefix.reserveCapacity(values.count + 1)
        for value in values { prefix.append(prefix[prefix.count - 1] + value) }
        let half = window / 2
        return values.indices.map { index in
            let low = max(0, index - half)
            let high = min(values.count, index + half + 1)
            return (prefix[high] - prefix[low]) / Double(high - low)
        }
    }

    private static func autocorrelation(_ values: [Double], maxLag: Int) -> [Double] {
        let count = values.count
        let lagCap = min(maxLag, count - 1)
        var result = [Double](repeating: 0, count: maxLag + 1)
        for lag in 0...lagCap {
            var sum = 0.0
            for index in 0..<(count - lag) {
                sum += values[index] * values[index + lag]
            }
            result[lag] = sum / Double(count - lag)
        }
        return result
    }

    private static func variance(_ values: [Double]) -> Double {
        guard !values.isEmpty else { return 0 }
        let mean = values.reduce(0, +) / Double(values.count)
        let squared = values.reduce(0) { $0 + ($1 - mean) * ($1 - mean) }
        return squared / Double(values.count)
    }
}

struct AutoGaitCanceller {
    private var buffer: [(point: CGPoint, time: TimeInterval)] = []
    private var stabilizer = GaitStabilizer()
    private var engaged = false
    private var engagement: Double = 0
    private var currentHz: Double = 0
    private var lastAnalysis: TimeInterval = 0
    private var lastEngagedAt: TimeInterval = 0
    private var confidenceStreak = 0
    private var missStreak = 0

    private let windowSeconds: TimeInterval = 8
    private let analysisInterval: TimeInterval = 0.8
    private let engageConfidence = 0.40
    private let analysisRate = 30.0

    var detectedHz: Double? {
        engaged && engagement > 0.5 ? currentHz : nil
    }

    mutating func reset() {
        buffer.removeAll(keepingCapacity: true)
        stabilizer.reset()
        engaged = false
        engagement = 0
        currentHz = 0
        lastAnalysis = 0
        lastEngagedAt = 0
        confidenceStreak = 0
        missStreak = 0
    }

    mutating func process(point: CGPoint, now: TimeInterval) -> CGPoint {
        if let last = buffer.last, now - last.time > 1.5 {
            buffer.removeAll(keepingCapacity: true)
        }
        buffer.append((point, now))
        let horizon = now - windowSeconds
        while let first = buffer.first, first.time < horizon {
            buffer.removeFirst()
        }

        if now - lastAnalysis >= analysisInterval {
            analyze(now: now)
        }

        let target: Double = engaged ? 1 : 0
        engagement += (target - engagement) * 0.08

        guard engagement > 0.02, stabilizer.isConfigured else { return point }
        let corrected = stabilizer.process(point: point, now: now)
        return CGPoint(
            x: point.x + (corrected.x - point.x) * CGFloat(engagement),
            y: point.y + (corrected.y - point.y) * CGFloat(engagement)
        )
    }

    private mutating func analyze(now: TimeInterval) {
        lastAnalysis = now

        let recent = buffer.suffix(75)
        if recent.count >= 30 {
            let mean = recent.reduce(CGPoint.zero) { $0 + $1.point } / CGFloat(recent.count)
            let rms = sqrt(recent.reduce(0.0) { $0 + Double($1.point.distance(to: mean) * $1.point.distance(to: mean)) } / Double(recent.count))
            if rms < 0.008 {
                registerMiss()
                return
            }
        }

        let uniform = GaitAnalyzer.resample(
            points: buffer.map(\.point),
            times: buffer.map(\.time),
            targetRate: analysisRate
        )
        guard uniform.count >= 165 else { return }

        guard let profile = GaitAnalyzer.analyze(samples: uniform, sampleRate: analysisRate),
              profile.confidence >= engageConfidence
        else {
            registerMiss()
            return
        }

        missStreak = 0
        confidenceStreak += 1
        let warm = now - lastEngagedAt < 60
            && currentHz > 0
            && abs(profile.fundamentalHz - currentHz) / currentHz < 0.25
        guard confidenceStreak >= (warm ? 1 : 2) else { return }

        let frequencyShift = abs(profile.fundamentalHz - currentHz) / max(currentHz, 0.1)
        if !stabilizer.isConfigured || frequencyShift > 0.12 {
            stabilizer.configure(profile: profile)
        }
        currentHz = profile.fundamentalHz
        engaged = true
        lastEngagedAt = now
    }

    private mutating func registerMiss() {
        confidenceStreak = 0
        missStreak += 1
        if missStreak >= 2 {
            engaged = false
        }
    }
}

struct GaitStabilizer {
    private var harmonics: [Double] = []
    private var cosCoeffX: [Double] = []
    private var sinCoeffX: [Double] = []
    private var cosCoeffY: [Double] = []
    private var sinCoeffY: [Double] = []
    private var phase: [Double] = []
    private var maxCorrection: Double = 0
    private var lastTime: TimeInterval?
    private var configuredFor: GaitProfile?
    private var meanX: Double?
    private var meanY: Double?

    private let mu = 0.05
    private let leak = 0.0015
    private let meanRate = 0.05

    var isConfigured: Bool { !harmonics.isEmpty }

    mutating func configure(profile: GaitProfile) {
        guard configuredFor != profile else { return }
        configuredFor = profile
        harmonics = profile.harmonics
        let count = harmonics.count
        cosCoeffX = Array(repeating: 0, count: count)
        sinCoeffX = Array(repeating: 0, count: count)
        cosCoeffY = Array(repeating: 0, count: count)
        sinCoeffY = Array(repeating: 0, count: count)
        phase = Array(repeating: 0, count: count)
        maxCorrection = max(profile.amplitude * 2.5, 0.01)
        lastTime = nil
        meanX = nil
        meanY = nil
    }

    mutating func reset() {
        for index in cosCoeffX.indices { cosCoeffX[index] = 0; sinCoeffX[index] = 0 }
        for index in cosCoeffY.indices { cosCoeffY[index] = 0; sinCoeffY[index] = 0 }
        for index in phase.indices { phase[index] = 0 }
        lastTime = nil
        meanX = nil
        meanY = nil
    }

    mutating func process(point: CGPoint, now: TimeInterval) -> CGPoint {
        guard !harmonics.isEmpty else { return point }
        let dt = min(max(now - (lastTime ?? now), 1.0 / 120.0), 2.0)
        lastTime = now

        let inputX = Double(point.x)
        let inputY = Double(point.y)
        meanX = (meanX ?? inputX) + meanRate * (inputX - (meanX ?? inputX))
        meanY = (meanY ?? inputY) + meanRate * (inputY - (meanY ?? inputY))
        let acX = inputX - (meanX ?? inputX)
        let acY = inputY - (meanY ?? inputY)

        var predictX = 0.0
        var predictY = 0.0
        for index in harmonics.indices {
            let c = cos(phase[index])
            let s = sin(phase[index])
            predictX += cosCoeffX[index] * c + sinCoeffX[index] * s
            predictY += cosCoeffY[index] * c + sinCoeffY[index] * s
        }
        predictX = min(max(predictX, -maxCorrection), maxCorrection)
        predictY = min(max(predictY, -maxCorrection), maxCorrection)

        let lmsErrorX = acX - predictX
        let lmsErrorY = acY - predictY

        for index in harmonics.indices {
            let c = cos(phase[index])
            let s = sin(phase[index])
            cosCoeffX[index] = (1 - leak) * cosCoeffX[index] + mu * lmsErrorX * c
            sinCoeffX[index] = (1 - leak) * sinCoeffX[index] + mu * lmsErrorX * s
            cosCoeffY[index] = (1 - leak) * cosCoeffY[index] + mu * lmsErrorY * c
            sinCoeffY[index] = (1 - leak) * sinCoeffY[index] + mu * lmsErrorY * s
            phase[index] += 2 * Double.pi * harmonics[index] * dt
            if phase[index] > 2 * Double.pi { phase[index] -= 2 * Double.pi }
        }

        return CGPoint(x: inputX - predictX, y: inputY - predictY)
    }
}
