import type { Project, ProjectHost } from "@/features/workspace/types";
import type { Desktop } from "./nativeLink";

export const HOSTS = "monocode.mobile.hosts";
export const LEGACY_PAIRING = "monocode.mobile.pairing";
export type Hosts = {
  desktops: Record<string, Desktop>;
  activeFingerprint: string | null;
  lastUsedHostByProject?: Record<string, string>;
};

export type HostProjectCatalog = {
  fingerprint: string;
  name: string;
  projects: Project[];
};

/** The folder name is the cross-host project identity used by the phone. */
export function projectFolderKey(project: Pick<Project, "path" | "name">) {
  const path = project.path.replace(/[\\/]+$/, "");
  const basename = path.split(/[\\/]/).at(-1) || project.name;
  return basename.toLowerCase();
}

/** One project row per case-insensitive folder basename, retaining host-local IDs. */
export function mergeProjectsByFolder(catalogs: HostProjectCatalog[]): Project[] {
  const merged = new Map<string, Project>();
  for (const catalog of catalogs) {
    for (const project of catalog.projects) {
      const key = projectFolderKey(project);
      const { hosts: _, ...local } = project;
      const host: ProjectHost = {
        fingerprint: catalog.fingerprint,
        name: catalog.name,
        project: local,
      };
      const existing = merged.get(key);
      if (existing) existing.hosts!.push(host);
      else merged.set(key, { ...local, hosts: [host] });
    }
  }
  return [...merged.values()];
}

/** Preferred host if it still has the project, otherwise the first paired host. */
export function chooseProjectHost(
  project: Pick<Project, "hosts">,
  preferredFingerprint?: string,
) {
  const hosts = project.hosts ?? [];
  return hosts.find((host) => host.fingerprint === preferredFingerprint) ?? hosts[0];
}

/** Legacy data is kept until the new record has been saved successfully. */
export function restoreHosts(saved: string | null, legacy: string | null): Hosts {
  if (saved) {
    const value = JSON.parse(saved) as Hosts;
    if (!value.desktops || Array.isArray(value.desktops) ||
        typeof value.desktops !== "object" ||
        (value.lastUsedHostByProject !== undefined &&
          (!value.lastUsedHostByProject || Array.isArray(value.lastUsedHostByProject) ||
            typeof value.lastUsedHostByProject !== "object" ||
            !Object.values(value.lastUsedHostByProject).every((host) => typeof host === "string"))) ||
        !Object.entries(value.desktops).every(([fingerprint, desktop]) =>
          desktop && desktop.fingerprint === fingerprint &&
          typeof desktop.name === "string" && Array.isArray(desktop.addresses) &&
          typeof desktop.deviceId === "string") ||
        (value.activeFingerprint !== null &&
          (typeof value.activeFingerprint !== "string" ||
            !Object.hasOwn(value.desktops, value.activeFingerprint))))
      throw new Error("Invalid saved hosts");
    return value;
  }
  const desktop = legacy ? JSON.parse(legacy) as Desktop : null;
  return {
    desktops: desktop ? { [desktop.fingerprint]: desktop } : {},
    activeFingerprint: desktop?.fingerprint ?? null,
  };
}
