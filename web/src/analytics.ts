export interface AnalyticsEnvironment {
  readonly VITE_UMAMI_SCRIPT_URL?: string;
  readonly VITE_UMAMI_WEBSITE_ID?: string;
  readonly VITE_UMAMI_DOMAINS?: string;
}

interface AnalyticsConfig {
  readonly scriptUrl: string;
  readonly websiteId: string;
  readonly domains: string | null;
}

export type AnalyticsInstallResult =
  "disabled" | "invalid" | "already-installed" | "installed";

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

const normalizeDomains = (value: string | undefined): string | null => {
  if (value === undefined || value.trim() === "") {
    return null;
  }

  const domains = value
    .split(",")
    .map((domain) => domain.trim().toLowerCase())
    .filter((domain) => domain !== "");
  if (
    domains.length === 0 ||
    domains.some((domain) => {
      try {
        return new URL(`https://${domain}`).hostname !== domain;
      } catch {
        return true;
      }
    })
  ) {
    return null;
  }
  return [...new Set(domains)].join(",");
};

export const analyticsConfig = (
  environment: AnalyticsEnvironment,
): AnalyticsConfig | null => {
  const scriptUrlValue = environment.VITE_UMAMI_SCRIPT_URL?.trim();
  const websiteId = environment.VITE_UMAMI_WEBSITE_ID?.trim();
  if (scriptUrlValue === undefined || websiteId === undefined) {
    return null;
  }

  try {
    const scriptUrl = new URL(scriptUrlValue);
    if (
      scriptUrl.protocol !== "https:" ||
      scriptUrl.username !== "" ||
      scriptUrl.password !== "" ||
      scriptUrl.hash !== "" ||
      !UUID_PATTERN.test(websiteId)
    ) {
      return null;
    }
    const domains = normalizeDomains(environment.VITE_UMAMI_DOMAINS);
    if (
      environment.VITE_UMAMI_DOMAINS !== undefined &&
      environment.VITE_UMAMI_DOMAINS.trim() !== "" &&
      domains === null
    ) {
      return null;
    }
    return {
      scriptUrl: scriptUrl.href,
      websiteId,
      domains,
    };
  } catch {
    return null;
  }
};

export const installAnalytics = (
  environment: AnalyticsEnvironment,
  documentReference: Document = document,
): AnalyticsInstallResult => {
  const configuredValues = [
    environment.VITE_UMAMI_SCRIPT_URL,
    environment.VITE_UMAMI_WEBSITE_ID,
    environment.VITE_UMAMI_DOMAINS,
  ].some((value) => value !== undefined && value.trim() !== "");
  const config = analyticsConfig(environment);
  if (config === null) {
    return configuredValues ? "invalid" : "disabled";
  }
  if (
    documentReference.querySelector<HTMLScriptElement>(
      "script[data-atlas-analytics]",
    ) !== null
  ) {
    return "already-installed";
  }

  const script = documentReference.createElement("script");
  script.dataset.atlasAnalytics = "umami";
  script.dataset.websiteId = config.websiteId;
  if (config.domains !== null) {
    script.dataset.domains = config.domains;
  }
  script.defer = true;
  script.src = config.scriptUrl;
  documentReference.head.append(script);
  return "installed";
};
