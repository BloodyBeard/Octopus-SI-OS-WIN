import type { OctopusProviderInterface } from "../../electron";

type Props = {
  endpoint: string;
  token: string;
  providerInterfaceId: string;
  providerInterfaces: OctopusProviderInterface[];
  activeProvider?: OctopusProviderInterface;
  artifactTransfer: {
    status: string;
    reason: string;
    path: string;
  };
  providerInterfaceError: string;
  onEndpointChange: (value: string) => void;
  onTokenChange: (value: string) => void;
  onProviderInterfaceChange: (value: string) => void;
};

export function ChatTransportPanel({
  endpoint,
  token,
  providerInterfaceId,
  providerInterfaces,
  activeProvider,
  artifactTransfer,
  providerInterfaceError,
  onEndpointChange,
  onTokenChange,
  onProviderInterfaceChange,
}: Props) {
  return (
    <div className="chat-transport-panel">
      <label>
        <span>endpoint</span>
        <input value={endpoint} onChange={(event) => onEndpointChange(event.target.value)} spellCheck={false} />
      </label>
      <label>
        <span>provider interface</span>
        {providerInterfaces.length ? (
          <select value={providerInterfaceId} onChange={(event) => onProviderInterfaceChange(event.target.value)}>
            {providerInterfaces.map((provider) => (
              <option key={provider.id} value={provider.id}>{provider.label}</option>
            ))}
          </select>
        ) : (
          <input value={providerInterfaceId} onChange={(event) => onProviderInterfaceChange(event.target.value)} spellCheck={false} />
        )}
      </label>
      <label>
        <span>bearer token</span>
        <input value={token} onChange={(event) => onTokenChange(event.target.value)} spellCheck={false} type="password" />
      </label>
      <div className="chat-provider-kinds">
        <span>artifact transfer</span>
        <strong title={artifactTransfer.path || artifactTransfer.reason}>
          {artifactTransfer.status}{artifactTransfer.reason ? ` / ${artifactTransfer.reason}` : ""}
        </strong>
      </div>
      <div className="chat-provider-kinds">
        <span>handler ref</span>
        <strong>{activeProvider?.runtimeHandlerRef || "not projected"}</strong>
      </div>
    </div>
  );
}
