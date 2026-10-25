import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ConnectionProvider, useConnection } from "@/features/connection/ConnectionProvider";
import { HOSTS, LEGACY_PAIRING } from "@/features/connection/hosts";
import type { Desktop } from "@/features/connection/nativeLink";

const state = vi.hoisted(() => ({
  storage: new Map<string, string>(),
  identity: JSON.stringify({ certificate: "phone-cert", privateKey: "phone-key" }),
  request: vi.fn(), nativeCall: vi.fn(), setIdentity: vi.fn(), save: vi.fn(),
}));
vi.mock("react-native", () => ({
  Platform: { OS: "ios" },
  AppState: { currentState: "active", addEventListener: () => ({ remove() {} }) },
}));
vi.mock("expo-constants", () => ({ default: { deviceName: "Phone" } }));
vi.mock("expo-secure-store", () => ({
  getItemAsync: async () => state.identity,
  setItemAsync: state.setIdentity,
  WHEN_UNLOCKED_THIS_DEVICE_ONLY: "device-only",
}));
vi.mock("expo-sqlite/kv-store", () => ({ default: {
  getItem: async (key: string) => state.storage.get(key) ?? null,
  setItem: async (key: string, value: string) => void state.storage.set(key, value),
  setItemSync: (key: string, value: string) => {
    state.save(key, value);
    state.storage.set(key, value);
  },
  removeItem: async (key: string) => void state.storage.delete(key),
} }));
vi.mock("@/features/connection/nativeLink", () => ({
  request: state.request,
  nativeCall: state.nativeCall,
  parseInvitation: () => ({ identity: {}, fingerprint: "invite", addresses: [] }),
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const mac: Desktop = { name: "Mac", fingerprint: "mac", deviceId: "same-id", addresses: [] };
const fedora: Desktop = { name: "Fedora", fingerprint: "fedora", deviceId: "same-id", addresses: [] };
const catalog = (name: string) => ({ projects: [{ id: name }], monos: [], sessions: [] });
const chosen = (name: string) => ({ projectIds: [name], monoIds: [] });
const cacheKey = (host: Desktop, kind: string) => `monocode.mobile.${host.fingerprint}.${kind}`;
let connection: ReturnType<typeof useConnection>;
let root: ReturnType<typeof createRoot>;
function Probe() { connection = useConnection(); return null; }
const render = () => act(async () => root.render(<ConnectionProvider><Probe /></ConnectionProvider>));
function seed() {
  state.storage.set(HOSTS, JSON.stringify({ desktops: { mac, fedora }, activeFingerprint: "mac" }));
  for (const host of [mac, fedora]) {
    state.storage.set(cacheKey(host, "catalog"), JSON.stringify(catalog(host.name)));
    state.storage.set(cacheKey(host, "selection"), JSON.stringify(chosen(host.name)));
  }
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
beforeEach(() => {
  vi.clearAllMocks();
  state.storage.clear();
  state.save.mockReset();
  state.nativeCall.mockReset();
  state.request.mockImplementation(async (pairing, method) => method === "workspace.list"
    ? catalog(pairing.desktop.name) : { sessionId: pairing.desktop.name });
  root = createRoot(document.createElement("div"));
});
afterEach(() => act(() => root.unmount()));

it("migrates the single pairing without enrollment or changing identity/selection", async () => {
  state.storage.set(LEGACY_PAIRING, JSON.stringify(mac));
  state.storage.set(cacheKey(mac, "selection"), JSON.stringify(chosen("Mac")));
  await render();
  expect(connection.pairing?.desktop).toEqual(mac);
  expect(connection.selection).toEqual(chosen("Mac"));
  expect(JSON.parse(state.storage.get(HOSTS)!)).toEqual({ desktops: { mac }, activeFingerprint: "mac" });
  expect(state.storage.has(LEGACY_PAIRING)).toBe(false);
  expect(state.nativeCall).not.toHaveBeenCalled();
  expect(state.setIdentity).not.toHaveBeenCalled();
});

it("keeps legacy data if saving the migration fails", async () => {
  state.storage.set(LEGACY_PAIRING, JSON.stringify(mac));
  state.save.mockImplementation(() => { throw new Error("Disk full"); });
  await render();
  expect(state.storage.get(LEGACY_PAIRING)).toBe(JSON.stringify(mac));
  expect(connection.error).toContain("restore");
});

it("switches catalogs/choices, persists the active host, and targets new chats only there", async () => {
  seed();
  await render();
  const oldCommand = connection.command;
  await act(async () => connection.switchHost("fedora"));
  expect(connection.pairing?.desktop.name).toBe("Fedora");
  expect(connection.selection).toEqual(chosen("Fedora"));
  expect(connection.catalog).toEqual(catalog("Fedora"));
  expect(JSON.parse(state.storage.get(HOSTS)!).activeFingerprint).toBe("fedora");
  await expect(oldCommand("session.start")).rejects.toThrow("Host changed");
  await connection.command("session.start", { projectId: "Fedora" });
  expect(state.request).toHaveBeenLastCalledWith(
    expect.objectContaining({ desktop: fedora }), "session.start", { projectId: "Fedora" }, undefined,
  );
  await act(async () => connection.switchHost("mac"));
  expect(connection.selection).toEqual(chosen("Mac"));
  expect(connection.hosts).toHaveLength(2);
  act(() => root.unmount());
  root = createRoot(document.createElement("div"));
  await render();
  expect(connection.pairing?.desktop.fingerprint).toBe("mac");
  expect(connection.hosts).toHaveLength(2);
});

it("persists refreshed addresses without replacing other hosts and restores the last active host", async () => {
  seed();
  state.request.mockImplementation(async (pairing) => ({
    ...catalog(pairing.desktop.name),
    addresses: [{ host: `${pairing.desktop.fingerprint}.example`, port: 38416 }],
  }));
  await render();
  await act(async () => connection.switchHost("fedora"));
  const saved = JSON.parse(state.storage.get(HOSTS)!);
  expect(saved.desktops.mac.addresses[0].host).toBe("mac.example");
  expect(saved.desktops.fedora.addresses[0].host).toBe("fedora.example");
  expect(state.storage.get(HOSTS)).not.toContain("phone-key");
  act(() => root.unmount());
  root = createRoot(document.createElement("div"));
  await render();
  expect(connection.pairing?.desktop.fingerprint).toBe("fedora");
  expect(connection.hosts).toHaveLength(2);
});

it("shows the selected host's offline cache and ignores an old host's late refresh", async () => {
  seed();
  const oldRefresh = deferred<ReturnType<typeof catalog>>();
  state.request.mockImplementation((pairing) => pairing.desktop.fingerprint === "mac"
    ? oldRefresh.promise : Promise.reject(new Error("Offline")));
  await render();
  await act(async () => connection.switchHost("fedora"));
  await act(async () => connection.refresh());
  expect(connection.catalog).toEqual(catalog("Fedora"));
  expect(connection.connected).toBe(false);
  expect(connection.error).toBe("Offline");
  await act(async () => oldRefresh.resolve(catalog("Late Mac")));
  expect(connection.catalog).toEqual(catalog("Fedora"));
  expect(connection.connected).toBe(false);
});

it("adds/deduplicates hosts using the shared identity; failed or cancelled add keeps the old host", async () => {
  seed();
  await render();
  act(() => connection.addHost());
  act(() => connection.cancelAddHost());
  expect(connection.addingHost).toBe(false);
  expect(connection.hosts).toHaveLength(2);
  state.nativeCall.mockRejectedValueOnce(new Error("Expired"));
  act(() => connection.addHost());
  await act(async () => connection.pair("invite"));
  expect(connection.error).toBe("Expired");
  expect(connection.pairing?.desktop.fingerprint).toBe("mac");
  expect(connection.hosts).toHaveLength(2);
  state.nativeCall.mockImplementation(async ({ operation }) =>
    operation === "exchange" ? fedora : "proof");
  await act(async () => connection.pair("invite"));
  expect(connection.hosts).toHaveLength(2);
  expect(connection.pairing?.desktop.fingerprint).toBe("fedora");
  expect(connection.addingHost).toBe(false);
  expect(state.nativeCall).not.toHaveBeenCalledWith({ operation: "identity" });
  expect(state.setIdentity).toHaveBeenLastCalledWith(
    "monocode.mobile.identity", state.identity, { keychainAccessible: "device-only" },
  );
  const linux = { ...fedora, fingerprint: "linux", name: "Another host" };
  state.nativeCall.mockImplementation(async ({ operation }) => operation === "exchange" ? linux : "proof");
  await act(async () => connection.pair("invite"));
  expect(connection.hosts.map((host) => host.name)).toEqual(["Mac", "Fedora", "Another host"]);
});

it("removes only the requested host, selects a remaining host, and preserves identity and choices", async () => {
  seed();
  await render();
  await act(async () => connection.removeHost("fedora"));
  expect(connection.pairing?.desktop.fingerprint).toBe("mac");
  expect(connection.hosts.map((host) => host.fingerprint)).toEqual(["mac"]);
  expect(state.storage.has(cacheKey(fedora, "selection"))).toBe(true);
  await act(async () => connection.removeHost("mac"));
  expect(connection.pairing).toBeNull();
  expect(connection.hosts).toEqual([]);
  expect(state.storage.has(cacheKey(mac, "selection"))).toBe(true);
  expect(state.identity).toContain("phone-cert");
});

it("removing the active host selects the remaining host with its saved choices", async () => {
  seed();
  await render();
  await act(async () => connection.removeHost("mac"));
  expect(connection.pairing?.desktop.fingerprint).toBe("fedora");
  expect(connection.selection).toEqual(chosen("Fedora"));
  expect(connection.hosts).toEqual([fedora]);
});
