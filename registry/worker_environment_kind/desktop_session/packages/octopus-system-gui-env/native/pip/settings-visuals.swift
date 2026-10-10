import AppKit
import SwiftUI

@MainActor
private final class SettingsArrowState: ObservableObject {
    @Published var enlarged = false
    private var presented = false
    private var appeared = false
    private var hovering = false
    private var generation: UInt64 = 0
    private let spring = Animation.interpolatingSpring(mass: 1, stiffness: 200, damping: 11, initialVelocity: 0)

    func setPresented(_ value: Bool) {
        guard presented != value else { return }
        presented = value
        restart()
    }

    func setAppeared(_ value: Bool) {
        guard appeared != value else { return }
        appeared = value
        restart()
    }

    private func restart() {
        generation &+= 1
        hovering = false
        enlarged = false
        guard presented && appeared else { return }
        let token = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            self?.rise(token)
        }
    }

    private func current(_ token: UInt64) -> Bool {
        presented && appeared && token == generation
    }

    private func animate(_ value: Bool) {
        withAnimation(spring) { enlarged = value }
    }

    func hover(_ value: Bool) {
        hovering = value
        guard value && presented && appeared else { return }
        let token = generation
        animate(true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
            guard let self, self.current(token) else { return }
            self.animate(false)
        }
    }

    private func rise(_ token: UInt64) {
        guard current(token) else { return }
        if !hovering { animate(true) }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
            guard let self, self.current(token) else { return }
            if !self.hovering { self.animate(false) }
            DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
                self?.rise(token)
            }
        }
    }
}

private struct SettingsArrowView: View {
    let image: NSImage
    @ObservedObject var state: SettingsArrowState

    var body: some View {
        ZStack {
            Image(nsImage: image)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 28)
                .frame(maxHeight: .infinity, alignment: .bottom)
                .scaleEffect(x: state.enlarged ? 1.15 : 1, y: state.enlarged ? 1.6 : 1, anchor: .bottom)
                .shadow(color: .black.opacity(0.23), radius: 7, x: 0, y: 4)
                .offset(y: -10)
                .onHover { state.hover($0) }
        }
        .frame(width: 100, height: 100)
        .onAppear { state.setAppeared(true) }
        .onDisappear { state.setAppeared(false) }
    }
}

@MainActor
@objc(OCTSettingsVisuals)
public final class SettingsVisuals: NSObject {
    private let state: SettingsArrowState
    @objc public let materialView: NSView
    @objc public let arrowView: NSView

    @objc(initWithImage:)
    public init(image: NSImage) {
        let state = SettingsArrowState()
        self.state = state
        let material = NSHostingView(rootView: Color.clear.background(.regularMaterial))
        material.sizingOptions = []
        materialView = material
        let arrow = NSHostingView(rootView: SettingsArrowView(image: image, state: state))
        arrow.sizingOptions = []
        arrowView = arrow
        super.init()
    }

    @objc(setPresented:)
    public func setPresented(_ value: Bool) {
        state.setPresented(value)
    }
}
