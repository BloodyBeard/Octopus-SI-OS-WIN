import { Component, type ErrorInfo, type ReactNode } from "react";
import { OcButton } from "../../design-system/components";
import { OctopusLoadingState } from "../OctopusLoadingLogo";

export function SurfaceLoadingState({ label }: { label: string }) {
  return <OctopusLoadingState className="surface-loading-state h-full w-full" label={label} />;
}

export class SurfaceRenderBoundary extends Component<{
  children: ReactNode;
  resetKey: string;
}, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    void window.octopusTransport?.logClientEvent({
      level: "error",
      event: "renderer.surface_failure",
      message: `${error.stack || error.message}\n${info.componentStack ?? ""}`.slice(0, 8_000),
    }).catch(() => undefined);
  }

  componentDidUpdate(previous: Readonly<{ children: ReactNode; resetKey: string }>) {
    if (previous.resetKey !== this.props.resetKey && this.state.failed) this.setState({ failed: false });
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="surface-render-error" role="alert">
        <strong>Tab content couldn’t render</strong>
        <OcButton onClick={() => this.setState({ failed: false })} size="sm" variant="secondary">Try again</OcButton>
      </div>
    );
  }
}
