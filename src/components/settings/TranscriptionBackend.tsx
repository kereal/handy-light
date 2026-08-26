import React from "react";
import { useTranslation } from "react-i18next";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";

interface TranscriptionBackendProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const URL_PLACEHOLDER = "ws://127.0.0.1:8765";

export const TranscriptionBackend: React.FC<TranscriptionBackendProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const backend = getSetting("transcription_backend") ?? "local";
    const url = getSetting("websocket_proxy_url") ?? "";
    const backendUpdating = isUpdating("transcription_backend");
    const urlUpdating = isUpdating("websocket_proxy_url");

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
        )}
      </>
    );
  },
);
