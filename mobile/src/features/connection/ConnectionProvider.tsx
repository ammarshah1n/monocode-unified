import {
  createContext,
  use,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { AppState, Platform } from "react-native";
import Constants from "expo-constants";
import * as SecureStore from "expo-secure-store";
import Storage from "expo-sqlite/kv-store";
import type {
  Project,
  Mono,
  SessionSummary,
  Message,
  Turn,
} from "@/features/workspace/types";
import {
  nativeCall,
  parseInvitation,
  request,
  type Pairing,
  type Desktop,
  type Identity,
} from "./nativeLink";
import { normalizeSelection, type MobileSelection } from "./selection";
import {
  HOSTS,
  LEGACY_PAIRING,
  mergeProjectsByFolder,
  projectFolderKey,
  restoreHosts,
  type Hosts,
} from "./hosts";

export type Catalog = {
  projects: Project[];
  monos: Mono[];
  sessions: SessionSummary[];
  addresses?: Desktop["addresses"];
};
const EMPTY: Catalog = { projects: [], monos: [], sessions: [] };
const IDENTITY = "monocode.mobile.identity";
const key = (fingerprint: string, kind: string) =>
  `monocode.mobile.${fingerprint}.${kind}`;
type Connection = {
  restoring: boolean;
  pairing: Pairing | null;
  hosts: Desktop[];
  addingHost: boolean;
  hostCatalogs: Record<string, Catalog>;
  hostSelections: Record<string, MobileSelection | null>;
  lastUsedHostByProject: Record<string, string>;
  catalog: Catalog;
  selection: MobileSelection | null;
  connected: boolean;
  working: boolean;
  error: string | null;
  pair: (text: string) => Promise<void>;
  addHost: () => void;
  cancelAddHost: () => void;
  switchHost: (fingerprint: string) => Promise<void>;
  setProjectHost: (project: Project, fingerprint: string) => void;
  removeHost: (fingerprint: string) => Promise<void>;
  refresh: () => Promise<void>;
  select: (selection: MobileSelection) => Promise<void>;
  command: <T>(method: string, params?: object, id?: string) => Promise<T>;
  commandOnHost: <T>(fingerprint: string, method: string, params?: object, id?: string) => Promise<T>;
};
const Context = createContext<Connection | null>(null);
export function ConnectionProvider({ children }: { children: ReactNode }) {
  const [restoring, setRestoring] = useState(true);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [hosts, setHosts] = useState<Desktop[]>([]);
  const [addingHost, setAddingHost] = useState(false);
  const [hostCatalogs, setHostCatalogs] = useState<Record<string, Catalog>>({});
  const [hostSelections, setHostSelections] = useState<Record<string, MobileSelection | null>>({});
  const [lastUsedHostByProject, setLastUsedHostByProject] = useState<Record<string, string>>({});
  const [catalog, setCatalog] = useState<Catalog>(EMPTY);
  const [selection, setSelection] = useState<MobileSelection | null>(null);
  const [connected, setConnected] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const registry = useRef<Hosts>({ desktops: {}, activeFingerprint: null });
  const active = useRef<Pairing | null>(null);
  const refreshing = useRef(new Set<Pairing>());
  const lastCatalog = useRef<string | null>(null);
  const pairingNow = useRef(false);
  const addingHostRef = useRef(false);
  const generation = useRef(0);
  const loadedCatalogs = useRef(new Set<string>());
  const retryHostAt = useRef<Record<string, number>>({});
  // A single synchronous record keeps address refreshes and host switches atomic.
  const saveHosts = useCallback((next: Hosts) => {
    Storage.setItemSync(HOSTS, JSON.stringify(next));
    registry.current = next;
    setHosts(Object.values(next.desktops));
    setLastUsedHostByProject(next.lastUsedHostByProject ?? {});
  }, []);
  const activate = useCallback(async (value: Pairing | null, next: Hosts) => {
    const catalogs: Record<string, Catalog> = {};
    const selections: Record<string, MobileSelection | null> = {};
    await Promise.all(Object.values(next.desktops).map(async (desktop) => {
      const [savedCatalog, savedSelection] = await Promise.all([
        Storage.getItem(key(desktop.fingerprint, "catalog")),
        Storage.getItem(key(desktop.fingerprint, "selection")),
      ]);
      catalogs[desktop.fingerprint] = savedCatalog ? JSON.parse(savedCatalog) : EMPTY;
      selections[desktop.fingerprint] = savedSelection
        ? normalizeSelection(JSON.parse(savedSelection)) : null;
    }));
    const fingerprint = value?.desktop.fingerprint;
    const selection = fingerprint ? selections[fingerprint] ?? null : null;
    const catalog = fingerprint ? catalogs[fingerprint] ?? EMPTY : EMPTY;
    saveHosts(next);
    // Keep cached snapshots visible immediately; refresh every paired host once this run.
    loadedCatalogs.current = new Set();
    generation.current++;
    active.current = value;
    setConnected(false);
    setError(null);
    setHostCatalogs(catalogs);
    setHostSelections(selections);
    setCatalog(catalog);
    lastCatalog.current = fingerprint
      ? await Storage.getItem(key(fingerprint, "catalog")) : null;
    setSelection(selection);
    setPairing(value);
  }, [saveHosts]);
  useEffect(() => {
    let disposed = false;
    void (async () => {
      try {
        const [saved, legacy, identity] = await Promise.all([
          Storage.getItem(HOSTS),
          Storage.getItem(LEGACY_PAIRING),
          SecureStore.getItemAsync(IDENTITY),
        ]);
        if (disposed) return;
        const next = restoreHosts(saved, legacy);
        registry.current = next;
        setHosts(Object.values(next.desktops));
        const desktop = next.activeFingerprint
          ? next.desktops[next.activeFingerprint] : null;
        if (desktop && !identity) throw new Error("Device identity is missing");
        await activate(desktop && identity
          ? { desktop, identity: JSON.parse(identity) } : null, next);
        // Never remove the old pairing until migration has durably succeeded.
        if (legacy) await Storage.removeItem(LEGACY_PAIRING);
      } catch {
        if (!disposed)
          setError("Could not restore the desktop connection. Connect again.");
      } finally {
        if (!disposed) setRestoring(false);
      }
    })();
    return () => {
      disposed = true;
    };
  }, [activate]);
  const refresh = useCallback(async () => {
    const value = active.current;
    if (!value || refreshing.current.has(value)) return;
    const version = generation.current;
    refreshing.current.add(value);
    const update = async (desktop: Desktop, next: Catalog) => {
      if (active.current !== value || version !== generation.current ||
          !registry.current.desktops[desktop.fingerprint]) return;
      if (!Array.isArray(next.projects) || !Array.isArray(next.monos) ||
          !Array.isArray(next.sessions))
        throw new Error("Desktop sent an invalid workspace");
      retryHostAt.current[desktop.fingerprint] = 0;
      loadedCatalogs.current.add(desktop.fingerprint);
      if (next.addresses?.length &&
          JSON.stringify(next.addresses) !== JSON.stringify(desktop.addresses)) {
        const updated = { ...desktop, addresses: next.addresses };
        saveHosts({
          ...registry.current,
          desktops: { ...registry.current.desktops, [updated.fingerprint]: updated },
        });
        if (desktop.fingerprint === value.desktop.fingerprint) value.desktop = updated;
      }
      const { addresses: _, ...workspace } = next;
      const serialized = JSON.stringify(workspace);
      await Storage.setItem(key(desktop.fingerprint, "catalog"), serialized);
      if (active.current !== value || version !== generation.current ||
          !registry.current.desktops[desktop.fingerprint]) return;
      setHostCatalogs((previous) => ({
        ...previous,
        [desktop.fingerprint]: withUnchanged(previous[desktop.fingerprint] ?? EMPTY, next),
      }));
      if (desktop.fingerprint === value.desktop.fingerprint) {
        lastCatalog.current = serialized;
        setCatalog((previous) => withUnchanged(previous, next));
      }
    };
    const now = Date.now();
    const inactive = Object.values(registry.current.desktops).filter((desktop) =>
      desktop.fingerprint !== value.desktop.fingerprint &&
      !loadedCatalogs.current.has(desktop.fingerprint) &&
      (retryHostAt.current[desktop.fingerprint] ?? 0) <= now,
    );
    // Cache paired hosts in the background; an unreachable spare must not block the active host.
    void Promise.all(inactive.map(async (desktop) => {
      try {
        const next = await request<Catalog>({ desktop, identity: value.identity }, "workspace.list");
        await update(desktop, next);
      } catch {
        retryHostAt.current[desktop.fingerprint] = Date.now() + 30_000;
      }
    }));
    try {
      const next = await request<Catalog>(value, "workspace.list");
      await update(value.desktop, next);
      if (active.current === value && version === generation.current) {
        setConnected(true);
        if (!addingHostRef.current) setError(null);
      }
    } catch (reason) {
      if (active.current === value) {
        setConnected(false);
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    } finally {
      refreshing.current.delete(value);
    }
  }, [saveHosts]);
  useEffect(() => {
    if (!pairing || addingHost || working) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (AppState.currentState === "active") await refresh();
      if (!disposed) timer = setTimeout(() => void poll(), 5000);
    };
    void poll();
    const subscription = AppState.addEventListener("change", (state) => {
      if (state === "active") void refresh();
    });
    return () => {
      disposed = true;
      clearTimeout(timer);
      subscription.remove();
    };
  }, [pairing, addingHost, working, refresh]);
  const pair = useCallback(async (text: string) => {
    if (pairingNow.current) return;
    pairingNow.current = true;
    setWorking(true);
    setError(null);
    try {
      const invitation = parseInvitation(text);
      const existing = await SecureStore.getItemAsync(IDENTITY);
      const identity = existing
        ? (JSON.parse(existing) as Identity)
        : await nativeCall<Identity>({ operation: "identity" });
      await SecureStore.setItemAsync(IDENTITY, JSON.stringify(identity), {
        keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
      });
      const fingerprint = await nativeCall<string>({
        operation: "fingerprint",
        identity: invitation.identity,
      });
      const proof = await nativeCall<string>({
        operation: "proof",
        identity,
        invitation: fingerprint,
      });
      const desktop = await nativeCall<Desktop>({
        operation: "exchange",
        identity: invitation.identity,
        pin: invitation.fingerprint,
        addresses: invitation.addresses,
        request: {
          version: 1,
          id: "enroll",
          method: "device.enroll",
          params: {
            certificate: identity.certificate,
            proof,
            name: deviceName(),
          },
        },
      });
      await activate({ desktop, identity }, {
        desktops: { ...registry.current.desktops, [desktop.fingerprint]: desktop },
        activeFingerprint: desktop.fingerprint,
      });
      addingHostRef.current = false;
      setAddingHost(false);
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      pairingNow.current = false;
      setWorking(false);
    }
  }, [activate, refresh]);
  const select = useCallback(async (value: MobileSelection) => {
    if (!pairing || active.current !== pairing) return;
    const catalogs = Object.values(registry.current.desktops).map((host) => ({
      fingerprint: host.fingerprint,
      name: host.name,
      projects: hostCatalogs[host.fingerprint]?.projects ?? [],
    }));
    const projects = mergeProjectsByFolder(catalogs);
    const chosen = new Set(value.projectIds);
    const folders = new Set(projects.filter((project) =>
      chosen.has(project.id) || project.hosts?.some((host) => chosen.has(host.project.id)),
    ).map(projectFolderKey));
    const next: Record<string, MobileSelection | null> = { ...hostSelections };
    await Promise.all(Object.values(registry.current.desktops).map(async (host) => {
      const saved = next[host.fingerprint];
      const hostCatalog = hostCatalogs[host.fingerprint] ?? EMPTY;
      const selectedIds = hostCatalog.projects
        .filter((project) => folders.has(projectFolderKey(project)))
        .map((project) => project.id);
      const known = new Set(hostCatalog.projects.map((project) => project.id));
      next[host.fingerprint] = {
        projectIds: [...new Set([
          ...selectedIds,
          ...(saved?.projectIds ?? []).filter((id) => !known.has(id)),
        ])],
        monoIds: host.fingerprint === pairing.desktop.fingerprint
          ? value.monoIds : saved?.monoIds ?? [],
      };
      await Storage.setItem(
        key(host.fingerprint, "selection"),
        JSON.stringify(next[host.fingerprint]),
      );
    }));
    if (active.current === pairing) {
      setHostSelections(next);
      setSelection(next[pairing.desktop.fingerprint] ?? value);
    }
  }, [hostCatalogs, hostSelections, pairing]);
  const switchHost = useCallback(async (fingerprint: string) => {
    if (pairingNow.current || active.current?.desktop.fingerprint === fingerprint) return;
    const desktop = registry.current.desktops[fingerprint];
    if (!desktop) throw new Error("This host is no longer paired");
    pairingNow.current = true;
    setWorking(true);
    try {
      const identity = await SecureStore.getItemAsync(IDENTITY);
      if (!identity) throw new Error("Device identity is missing");
      await activate({ desktop, identity: JSON.parse(identity) }, {
        ...registry.current, activeFingerprint: fingerprint,
      });
    } finally {
      pairingNow.current = false;
      setWorking(false);
    }
  }, [activate]);
  const setProjectHost = useCallback((project: Project, fingerprint: string) => {
    if (!project.hosts?.some((host) => host.fingerprint === fingerprint) ||
        !registry.current.desktops[fingerprint])
      throw new Error("This host does not have the project");
    saveHosts({
      ...registry.current,
      lastUsedHostByProject: {
        ...registry.current.lastUsedHostByProject,
        [projectFolderKey(project)]: fingerprint,
      },
    });
  }, [saveHosts]);
  const removeHost = useCallback(async (fingerprint: string) => {
    if (pairingNow.current) return;
    pairingNow.current = true;
    setWorking(true);
    try {
      const desktops = { ...registry.current.desktops };
      delete desktops[fingerprint];
      if (active.current?.desktop.fingerprint === fingerprint) {
        const desktop = Object.values(desktops)[0];
        await activate(desktop && active.current
          ? { desktop, identity: active.current.identity } : null, {
          desktops, activeFingerprint: desktop?.fingerprint ?? null,
        });
      } else {
        saveHosts({ ...registry.current, desktops });
      }
      // Forget locally, without connecting to an inactive host to revoke it.
      // Keep the shared identity and fingerprint-scoped workspace choices.
    } finally {
      pairingNow.current = false;
      setWorking(false);
    }
  }, [activate, saveHosts]);
  const addHost = useCallback(() => {
    if (pairingNow.current) return;
    addingHostRef.current = true;
    setError(null);
    setAddingHost(true);
  }, []);
  const cancelAddHost = useCallback(() => {
    if (!pairingNow.current) {
      addingHostRef.current = false;
      setError(null);
      setAddingHost(false);
    }
  }, []);
  const command = useCallback(
    async <T,>(
      method: string,
      params: object = {},
      id?: string,
    ): Promise<T> => {
      // A callback retained by an old screen must never send to the new host.
      if (!pairing || active.current !== pairing)
        throw new Error("Host changed. Open the conversation on the active host.");
      return request<T>(pairing, method, params, id);
    },
    [pairing],
  );
  const commandOnHost = useCallback(async <T,>(
    fingerprint: string,
    method: string,
    params: object = {},
    id?: string,
  ): Promise<T> => {
    const desktop = registry.current.desktops[fingerprint];
    const identity = active.current?.identity;
    if (!desktop || !identity) throw new Error("This host is no longer paired");
    return request<T>({ desktop, identity }, method, params, id);
  }, []);
  // Every screen reads this; a new object each render would re-render them all.
  const value = useMemo(
    () => ({
      restoring,
      pairing,
      hosts,
      addingHost,
      hostCatalogs,
      hostSelections,
      lastUsedHostByProject,
      catalog,
      selection,
      connected,
      working,
      error,
      pair,
      addHost,
      cancelAddHost,
      switchHost,
      setProjectHost,
      removeHost,
      refresh,
      select,
      command,
      commandOnHost,
    }),
    [
      restoring,
      pairing,
      hosts,
      addingHost,
      hostCatalogs,
      hostSelections,
      lastUsedHostByProject,
      catalog,
      selection,
      connected,
      working,
      error,
      pair,
      addHost,
      cancelAddHost,
      switchHost,
      setProjectHost,
      removeHost,
      refresh,
      select,
      command,
      commandOnHost,
    ],
  );
  return <Context value={value}>{children}</Context>;
}
/**
 * The new catalog, keeping the projects, monos and sessions that did not
 * change as they were, so rows memoized on them skip re-rendering.
 */
function withUnchanged(previous: Catalog, next: Catalog): Catalog {
  return {
    ...next,
    projects: sameItems(previous.projects, next.projects),
    monos: sameItems(previous.monos, next.monos),
    sessions: sameItems(previous.sessions, next.sessions),
  };
}
function sameItems<T extends { id: string }>(previous: T[], next: T[]): T[] {
  const known = new Map(previous.map((item) => [item.id, item]));
  const merged = next.map((item) => {
    const before = known.get(item.id);
    return before && JSON.stringify(before) === JSON.stringify(item) ? before : item;
  });
  return merged.length === previous.length && merged.every((item, i) => item === previous[i])
    ? previous
    : merged;
}
export function useConnection() {
  const value = use(Context);
  if (!value) throw new Error("ConnectionProvider is missing");
  return value;
}
export type Conversation = {
  id: string;
  messages: Message[];
  /** Missing from desktops that predate turn-shaped transcripts. */
  turns?: Turn[];
  busy: boolean;
};

/** What the desktop lists this phone as, e.g. "Nick's iPhone". */
function deviceName() {
  return (
    Constants.deviceName?.trim() || (Platform.OS === "ios" ? "iPhone" : "Phone")
  );
}
