import { Component, type ErrorInfo, type ReactNode } from "react";
import { OcButton } from "../../design-system/components";

type Props = {
  children: ReactNode;
  compact?: boolean;
  detail: string;
  resetKey: string;
  title: string;
};

type State = {
  componentStack: string;
  error: Error | null;
};

export class ChatRenderBoundary extends Component<Props, State> {
  state: State = { componentStack: "", error: null };

  static getDerivedStateFromError(error: Error): State {
    return { componentStack: "", error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    const componentStack = info.componentStack ?? "";
    this.setState({ componentStack });
    void window.octopusTransport?.logClientEvent({
      level: "error",
      event: "renderer.chat_surface_failure",
      message: `${this.props.title}: ${error.stack || error.message}\n${componentStack}`.slice(0, 8_000),
    }).catch(() => undefined);
  }

  componentDidUpdate(previous: Props) {
    if (previous.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ componentStack: "", error: null });
    }
  }

  render() {
    const { error, componentStack } = this.state;
    if (!error) return this.props.children;
    const detail = `${error.stack || error.message}\n${componentStack}`.trim();
    return (
      <section className={`chat-render-failure${this.props.compact ? " is-compact" : ""}`} role="alert">
        <div className="chat-render-failure-copy">
          <strong>{this.props.title}</strong>
          <span>{this.props.detail}</span>
        </div>
        <div className="chat-render-failure-actions">
          <OcButton onClick={() => void window.octopusTransport?.copyText(detail)} size="xs" variant="ghost">Copy details</OcButton>
          <OcButton onClick={() => this.setState({ componentStack: "", error: null })} size="xs" variant="secondary">Try again</OcButton>
        </div>
      </section>
    );
  }
}
