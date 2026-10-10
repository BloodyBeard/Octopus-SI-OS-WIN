import AVFoundation
import CoreMedia
import Foundation
import ImageIO
import QuartzCore
import Vision

final class HandPoseDetector {
    private let request: VNDetectHumanHandPoseRequest = {
        let request = VNDetectHumanHandPoseRequest()
        request.maximumHandCount = 1
        return request
    }()

    func detect(in sampleBuffer: CMSampleBuffer, minimumConfidence: Float) throws -> HandPoseFrame? {
        let handler = VNImageRequestHandler(
            cmSampleBuffer: sampleBuffer,
            orientation: .up,
            options: [:]
        )

        return try detect(with: handler, minimumConfidence: minimumConfidence)
    }

    func detect(in image: CGImage, minimumConfidence: Float) throws -> HandPoseFrame? {
        let handler = VNImageRequestHandler(
            cgImage: image,
            orientation: .up,
            options: [:]
        )

        return try detect(with: handler, minimumConfidence: minimumConfidence)
    }

    private func detect(with handler: VNImageRequestHandler, minimumConfidence: Float) throws -> HandPoseFrame? {
        try handler.perform([request])
        guard let observation = request.results?.first else {
            return nil
        }

        let recognized = try observation.recognizedPoints(.all)
        var points: [HandJoint: HandPoint] = [:]

        for joint in HandJoint.allCases {
            guard let visionJoint = joint.visionName,
                  let point = recognized[visionJoint],
                  point.confidence >= minimumConfidence
            else {
                continue
            }
            points[joint] = HandPoint(location: point.location, confidence: point.confidence)
        }

        guard points.count >= 12 else { return nil }
        return HandPoseFrame(points: points, timestamp: CACurrentMediaTime())
    }
}

private extension HandJoint {
    var visionName: VNHumanHandPoseObservation.JointName? {
        switch self {
        case .wrist: return .wrist
        case .thumbCMC: return .thumbCMC
        case .thumbMP: return .thumbMP
        case .thumbIP: return .thumbIP
        case .thumbTip: return .thumbTip
        case .indexMCP: return .indexMCP
        case .indexPIP: return .indexPIP
        case .indexDIP: return .indexDIP
        case .indexTip: return .indexTip
        case .middleMCP: return .middleMCP
        case .middlePIP: return .middlePIP
        case .middleDIP: return .middleDIP
        case .middleTip: return .middleTip
        case .ringMCP: return .ringMCP
        case .ringPIP: return .ringPIP
        case .ringDIP: return .ringDIP
        case .ringTip: return .ringTip
        case .littleMCP: return .littleMCP
        case .littlePIP: return .littlePIP
        case .littleDIP: return .littleDIP
        case .littleTip: return .littleTip
        }
    }
}
