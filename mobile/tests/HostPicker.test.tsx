import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HostChip, HostsSection, ProjectHostChip } from "@/features/connection/HostPicker";
import type { Project } from "@/features/workspace/types";

const state = vi.hoisted(() => ({
  connected: true,
  working: false,
  switchHost: vi.fn(), setProjectHost: vi.fn(), addHost: vi.fn(), removeHost: vi.fn(), alert: vi.fn(),
}));
vi.mock("react-native", () => ({
  StyleSheet: { hairlineWidth: 1 },
  View: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  Text: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
  ScrollView: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  Pressable: ({ children, onPress, accessibilityLabel, disabled }: {
    children?: ReactNode; onPress?: () => void; accessibilityLabel?: string; disabled?: boolean;
  }) => <button aria-label={accessibilityLabel} onClick={onPress} disabled={disabled}>{children}</button>,
  Modal: ({ visible, children }: { visible: boolean; children?: ReactNode }) => visible ? <section>{children}</section> : null,
  ActivityIndicator: () => <span>Working</span>,
  Alert: { alert: state.alert },
}));
vi.mock("@/features/settings/SettingsUI", () => ({
  SettingsRow: ({ label, value, onPress }: { label: string; value?: string; onPress: () => void }) =>
    <button onClick={onPress}>{label} {value}</button>,
}));
vi.mock("@/ui/Icon", () => ({ Icon: () => null }));
vi.mock("@/ui/theme", () => ({
  themedStyles: (create: (colors: object) => object) => () => create({}),
  useColors: () => ({}), radii: { card: 20 },
}));
vi.mock("@/features/connection/ConnectionProvider", () => ({
  useConnection: () => ({
    pairing: { desktop: { fingerprint: "mac", name: "Mac" } },
    hosts: [{ fingerprint: "mac", name: "Mac" }, { fingerprint: "fedora", name: "Fedora" }],
    connected: state.connected, working: state.working, error: state.connected ? null : "Offline",
    switchHost: state.switchHost, setProjectHost: state.setProjectHost,
    lastUsedHostByProject: { ops: "fedora" }, addHost: state.addHost, removeHost: state.removeHost,
  }),
}));
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
beforeEach(() => {
  vi.clearAllMocks();
  state.connected = true;
  state.working = false;
  state.switchHost.mockResolvedValue(undefined);
  state.setProjectHost.mockReset();
  state.removeHost.mockResolvedValue(undefined);
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(() => act(() => root.unmount()));
const button = (text: string) => [...container.querySelectorAll("button")].find((element) =>
  element.textContent?.startsWith(text))!;

const project: Project = {
  id: "mac-ops", name: "Ops", path: "/Users/me/Ops", machine: "", branch: "main",
  color: "blue", additions: 0, deletions: 0,
  hosts: [
    { fingerprint: "mac", name: "Mac", project: { id: "mac-ops", name: "Ops", path: "/Users/me/Ops", machine: "", branch: "main", color: "blue", additions: 0, deletions: 0 } },
    { fingerprint: "fedora", name: "Fedora", project: { id: "fedora-ops", name: "Ops", path: "/home/me/Ops", machine: "", branch: "main", color: "blue", additions: 0, deletions: 0 } },
  ],
};

it("uses and persists the preferred project host through the existing switch", async () => {
  act(() => root.render(<ProjectHostChip project={project} />));
  expect(container.textContent).toContain("Fedora");
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label^="Host:"]')!.click());
  await act(async () => button("Mac").click());
  expect(state.setProjectHost).toHaveBeenCalledWith(project, "mac");
  expect(state.switchHost).toHaveBeenCalledWith("mac");
});

it("keeps the active host visible and opens a host picker with honest connection status", async () => {
  act(() => root.render(<HostChip />));
  expect(container.textContent).toBe("Mac");
  await act(async () => button("Mac").click());
  expect(container.textContent).toContain("Mac Active · Connected");
  expect(container.textContent).toContain("Fedora Not connected");
  await act(async () => button("Fedora").click());
  expect(state.switchHost).toHaveBeenCalledWith("fedora");
  expect(container.querySelector("section")).toBeNull();
});

it("shows unreachable only for the active host and reuses the add-host flow", () => {
  state.connected = false;
  act(() => root.render(<HostsSection />));
  expect(container.textContent).toContain("Mac Active · Unreachable");
  act(() => button("Add host").click());
  expect(state.addHost).toHaveBeenCalledOnce();
});

it("confirms removal of a specific host without removing others", async () => {
  act(() => root.render(<HostsSection />));
  act(() => container.querySelector<HTMLButtonElement>('[aria-label="Remove host Fedora"]')!.click());
  expect(state.removeHost).not.toHaveBeenCalled();
  const actions = state.alert.mock.calls[0][2] as { text: string; onPress?: () => void }[];
  await act(async () => actions.find((action) => action.text === "Remove host")!.onPress!());
  expect(state.removeHost).toHaveBeenCalledWith("fedora");
});
