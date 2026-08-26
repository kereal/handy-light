import React from "react";
import { useTranslation } from "react-i18next";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";

interface TranscriptionBackendProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const URL_PLACEHOLDER = "ws://127.0.0.1:8765";

export const TranscriptionBackend: React.FC<TranscriptionBackendProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating, settings, refreshSettings } =
      useSettings();

    const backend = getSetting("transcription_backend") ?? "local";
    const url = getSetting("websocket_proxy_url") ?? "";
    const token = settings?.post_process_api_keys?.["websocket_proxy"] ?? "";
    const backendUpdating = isUpdating("transcription_backend");
    const urlUpdating = isUpdating("websocket_proxy_url");
    const [tokenUpdating, setTokenUpdating] = React.useState(false);
    const [tokenDraft, setTokenDraft] = React.useState(token);

    // Keep local draft in sync if the store value changes (initial load,
    // external reset, or another tab in the future).
    React.useEffect(() => {
      setTokenDraft(token);
    }, [token]);

    // Persist the bearer token only on blur / Enter to avoid hammering the
    // Tauri command for every keystroke. After the write, the store must
    // re-fetch — `post_process_api_keys` is a SecretMap, not pushed to the
    // client cache as eagerly as scalar settings.
    const onTokenCommit = async (value: string) => {
      setTokenUpdating(true);
      try {
        const result = await commands.setWebsocketProxyTokenSetting(value);
        if (result.status === "ok") {
          await refreshSettings();
        }
      } finally {
        setTokenUpdating(false);
      }
    };

    return (
      <>
        <SettingContainer
          title={t("settings.transcriptionBackend.title")}
          description={t("settings.transcriptionBackend.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div
            className="flex gap-1 p-1 rounded-md bg-mid-gray/10 border border-mid-gray/40"
            role="radiogroup"
            aria-label={t("settings.transcriptionBackend.title")}
          >
            <button
              type="button"
              role="radio"
              aria-checked={backend === "local"}
              disabled={backendUpdating}
              onClick={() => updateSetting("transcription_backend", "local")}
              className={`px-3 py-1 rounded text-xs font-medium transition-colors ${
                backend === "local"
                  ? "bg-logo-primary text-white"
                  : "text-mid-gray hover:text-text"
              } ${backendUpdating ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
            >
              {t("settings.transcriptionBackend.options.local")}
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={backend === "websocket_proxy"}
              disabled={backendUpdating}
              onClick={() =>
                updateSetting("transcription_backend", "websocket_proxy")
              }
              className={`px-3 py-1 rounded text-xs font-medium transition-colors ${
                backend === "websocket_proxy"
                  ? "bg-logo-primary text-white"
                  : "text-mid-gray hover:text-text"
              } ${backendUpdating ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
            >
              {t("settings.transcriptionBackend.options.websocketProxy")}
            </button>
          </div>
        </SettingContainer>
        {backend === "websocket_proxy" && (
          <>
            <SettingContainer
              title={t("settings.transcriptionBackend.urlTitle")}
              description={t("settings.transcriptionBackend.urlDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="stacked"
            >
              <Input
                type="text"
                value={url}
                disabled={urlUpdating}
                placeholder={URL_PLACEHOLDER}
                className="w-full"
                spellCheck={false}
                autoCorrect="off"
                autoCapitalize="off"
                onChange={(e) =>
                  updateSetting("websocket_proxy_url", e.target.value)
                }
              />
            </SettingContainer>
            <SettingContainer
              title={t("settings.transcriptionBackend.tokenTitle")}
              description={t("settings.transcriptionBackend.tokenDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="stacked"
            >
              <Input
                type="password"
                value={tokenDraft}
                disabled={tokenUpdating}
                placeholder={t("settings.transcriptionBackend.tokenPlaceholder")}
                className="w-full"
                spellCheck={false}
                autoCorrect="off"
                autoCapitalize="off"
                onChange={(e) => setTokenDraft(e.target.value)}
                onBlur={(e) => onTokenCommit(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.currentTarget.blur();
                  }
                }}
              />
            </SettingContainer>
          </>
        )}
      </>
    );
  },
);
