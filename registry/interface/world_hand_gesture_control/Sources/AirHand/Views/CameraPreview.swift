import AVFoundation
import SwiftUI

struct CameraPreview: NSViewRepresentable {
    let session: AVCaptureSession
    var mirrorX: Bool

    func makeNSView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.previewLayer.session = session
        view.previewLayer.videoGravity = .resizeAspect
        return view
    }

    func updateNSView(_ nsView: PreviewView, context: Context) {
        nsView.previewLayer.session = session
        if let connection = nsView.previewLayer.connection, connection.isVideoMirroringSupported {
            connection.automaticallyAdjustsVideoMirroring = false
            connection.isVideoMirrored = mirrorX
        }
    }
}

final class PreviewView: NSView {
    private let captureLayer = AVCaptureVideoPreviewLayer()

    var previewLayer: AVCaptureVideoPreviewLayer {
        captureLayer
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer = captureLayer
        captureLayer.videoGravity = .resizeAspect
        captureLayer.masksToBounds = true
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        wantsLayer = true
        layer = captureLayer
        captureLayer.videoGravity = .resizeAspect
        captureLayer.masksToBounds = true
    }

    override func layout() {
        super.layout()
        captureLayer.frame = bounds
    }
}

struct LandmarkOverlay: View {
    var landmarks: [HandJoint: CGPoint]
    var mirrorX: Bool
    var videoAspectRatio: Double

    var body: some View {
        GeometryReader { proxy in
            Canvas { context, size in
                let videoRect = fittedVideoRect(in: size)
                let mapped = landmarks.mapValues { point in
                    CGPoint(
                        x: videoRect.minX + (mirrorX ? 1 - point.x : point.x) * videoRect.width,
                        y: videoRect.minY + (1 - point.y) * videoRect.height
                    )
                }

                drawConnections(in: context, points: mapped)
                drawPoints(in: context, points: mapped)
            }
            .allowsHitTesting(false)
        }
    }

    private func fittedVideoRect(in size: CGSize) -> CGRect {
        let safeAspectRatio = max(CGFloat(videoAspectRatio), 0.01)
        let viewAspectRatio = max(size.width / max(size.height, 0.01), 0.01)

        if viewAspectRatio > safeAspectRatio {
            let width = size.height * safeAspectRatio
            return CGRect(x: (size.width - width) / 2, y: 0, width: width, height: size.height)
        }

        let height = size.width / safeAspectRatio
        return CGRect(x: 0, y: (size.height - height) / 2, width: size.width, height: height)
    }

    private func drawConnections(in context: GraphicsContext, points: [HandJoint: CGPoint]) {
        let chains: [[HandJoint]] = [
            [.wrist, .thumbCMC, .thumbMP, .thumbIP, .thumbTip],
            [.wrist, .indexMCP, .indexPIP, .indexDIP, .indexTip],
            [.wrist, .middleMCP, .middlePIP, .middleDIP, .middleTip],
            [.wrist, .ringMCP, .ringPIP, .ringDIP, .ringTip],
            [.wrist, .littleMCP, .littlePIP, .littleDIP, .littleTip],
            [.indexMCP, .middleMCP, .ringMCP, .littleMCP]
        ]

        for chain in chains {
            var path = Path()
            var started = false

            for joint in chain {
                guard let point = points[joint] else {
                    started = false
                    continue
                }

                if started {
                    path.addLine(to: point)
                } else {
                    path.move(to: point)
                    started = true
                }
            }

            context.stroke(path, with: .color(.green.opacity(0.72)), lineWidth: 2)
        }
    }

    private func drawPoints(in context: GraphicsContext, points: [HandJoint: CGPoint]) {
        for (joint, point) in points {
            let radius: CGFloat = joint == .wrist ? 5 : 3.5
            let rect = CGRect(x: point.x - radius, y: point.y - radius, width: radius * 2, height: radius * 2)
            context.fill(Path(ellipseIn: rect), with: .color(.white.opacity(0.95)))
            context.stroke(Path(ellipseIn: rect), with: .color(.black.opacity(0.55)), lineWidth: 1)
        }
    }
}
