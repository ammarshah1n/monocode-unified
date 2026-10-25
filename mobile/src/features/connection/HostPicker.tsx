import { Fragment, useState } from "react";
import {
  ActivityIndicator,
  Alert,
  Modal,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from "react-native";
import { SettingsRow } from "@/features/settings/SettingsUI";
import { Icon } from "@/ui/Icon";
import { radii, themedStyles, useColors } from "@/ui/theme";
import type { Project } from "@/features/workspace/types";
import { chooseProjectHost, projectFolderKey } from "./hosts";
import { useConnection } from "./ConnectionProvider";

const hostFailure = (reason: unknown) => Alert.alert("Could not change hosts", String(reason));

/** Inactive hosts are not probed: only the chosen host has a live connection. */
export function HostsSection({ onChoose }: { onChoose?: () => void }) {
  const connection = useConnection();
  const styles = useStyles();
  const colors = useColors();
  return (
    <View style={styles.card}>
      {connection.hosts.map((host) => {
        const active = host.fingerprint === connection.pairing?.desktop.fingerprint;
        const status = active
          ? connection.connected ? "Connected"
            : connection.working || !connection.error ? "Connecting…" : "Unreachable"
          : "Not connected";
        return (
          <Fragment key={host.fingerprint}>
            <SettingsRow
              icon={active ? "check" : "computer"}
              label={host.name}
              value={active ? `Active · ${status}` : status}
              showChevron={false}
              onPress={() => {
                if (connection.working) return;
                void connection.switchHost(host.fingerprint).then(onChoose).catch(hostFailure);
              }}
            />
            <Pressable
              accessibilityRole="button"
              accessibilityLabel={`Remove host ${host.name}`}
              disabled={connection.working}
              style={styles.remove}
              onPress={() => Alert.alert(
                `Remove ${host.name}?`,
                "This forgets the host on this phone. Workspace choices are kept. To connect again, scan a new code. Revoke this phone separately in the host’s settings.",
                [
                  { text: "Cancel", style: "cancel" },
                  { text: "Remove host", style: "destructive", onPress: () => {
                    void connection.removeHost(host.fingerprint).catch(hostFailure);
                  } },
                ],
              )}
            >
              <Text style={{ color: colors.deletions }}>Remove host</Text>
            </Pressable>
            <View style={styles.divider} />
          </Fragment>
        );
      })}
      <SettingsRow
        icon="add"
        label="Add host"
        showChevron={false}
        onPress={() => {
          if (connection.working) return;
          onChoose?.();
          connection.addHost();
        }}
      />
      {connection.working ? <ActivityIndicator color={colors.content} /> : null}
    </View>
  );
}

export function HostTags({ hosts }: { hosts: readonly { fingerprint: string; name: string }[] }) {
  const styles = useStyles();
  if (!hosts.length) return null;
  return (
    <View style={styles.tags}>
      {hosts.map((host) => (
        <View key={host.fingerprint} style={styles.tag}>
          <Text numberOfLines={1} style={styles.tagLabel}>{host.name}</Text>
        </View>
      ))}
    </View>
  );
}

/** Host choice scoped to this folder; only shown when more than one host has it. */
export function ProjectHostChip({ project }: { project: Project }) {
  const connection = useConnection();
  const styles = useStyles();
  const colors = useColors();
  const [open, setOpen] = useState(false);
  const key = projectFolderKey(project);
  const hosts = project.hosts ?? [];
  const selected = chooseProjectHost(project, connection.lastUsedHostByProject[key]);
  if (hosts.length < 2 || !selected) return null;
  return (
    <>
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={`Host: ${selected.name}. Choose host for new session`}
        accessibilityHint="Only machines with this project are listed"
        style={styles.projectChip}
        onPress={() => setOpen(true)}
      >
        <Icon name="computer" size={14} />
        <Text style={styles.hostLabel}>HOST</Text>
        <Text numberOfLines={1} style={styles.projectChipLabel}>{selected.name}</Text>
        <Icon name="chevronDown" size={12} />
      </Pressable>
      <Modal visible={open} transparent animationType="fade" onRequestClose={() => setOpen(false)}>
        <View style={styles.overlay}>
          <View style={styles.picker} accessibilityViewIsModal>
            <Text style={styles.title}>Run new session on</Text>
            <View style={styles.card}>
              {hosts.map((host, index) => (
                <Fragment key={host.fingerprint}>
                  {index ? <View style={styles.divider} /> : null}
                  <Pressable
                    accessibilityRole="radio"
                    accessibilityLabel={host.name}
                    accessibilityState={{ checked: selected.fingerprint === host.fingerprint }}
                    disabled={connection.working}
                    style={styles.hostOption}
                    onPress={() => {
                      setOpen(false);
                      try {
                        connection.setProjectHost(project, host.fingerprint);
                      } catch (reason) {
                        hostFailure(reason);
                        return;
                      }
                      void connection.switchHost(host.fingerprint).catch(hostFailure);
                    }}
                  >
                    <Icon name="computer" size={18} />
                    <Text style={styles.name}>{host.name}</Text>
                    {selected.fingerprint === host.fingerprint ? (
                      <Icon name="check" size={18} color={colors.accent} />
                    ) : null}
                  </Pressable>
                </Fragment>
              ))}
            </View>
            <Pressable accessibilityRole="button" onPress={() => setOpen(false)} style={styles.close}>
              <Text style={styles.name}>Close</Text>
            </Pressable>
          </View>
        </View>
      </Modal>
    </>
  );
}

export const HOST_CHIP_HEIGHT = 36;
export function HostChip() {
  const connection = useConnection();
  const styles = useStyles();
  const [open, setOpen] = useState(false);
  if (!connection.pairing) return null;
  return (
    <>
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={`Active host: ${connection.pairing.desktop.name}. Choose host`}
        style={styles.chip}
        hitSlop={4}
        onPress={() => setOpen(true)}
      >
        <Icon name={connection.connected ? "computer" : "computerOff"} size={16} />
        <Text numberOfLines={1} style={styles.name}>{connection.pairing.desktop.name}</Text>
        <Icon name="chevronDown" size={14} />
      </Pressable>
      <Modal visible={open} transparent animationType="fade" onRequestClose={() => setOpen(false)}>
        <View style={styles.overlay}>
          <View style={styles.picker} accessibilityViewIsModal>
            <Text style={styles.title}>Hosts</Text>
            <ScrollView><HostsSection onChoose={() => setOpen(false)} /></ScrollView>
            <Pressable accessibilityRole="button" onPress={() => setOpen(false)} style={styles.close}>
              <Text style={styles.name}>Close</Text>
            </Pressable>
          </View>
        </View>
      </Modal>
    </>
  );
}

const useStyles = themedStyles((colors) => ({
  chip: {
    height: HOST_CHIP_HEIGHT,
    maxWidth: "85%",
    alignSelf: "center",
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingHorizontal: 14,
    borderRadius: radii.card,
    backgroundColor: colors.surface,
    borderWidth: StyleSheet.hairlineWidth,
    borderColor: colors.stroke,
  },
  name: { color: colors.content, fontSize: 14, fontWeight: "600", flexShrink: 1 },
  card: { backgroundColor: colors.surface, borderRadius: radii.card, paddingHorizontal: 10, paddingVertical: 6 },
  tags: { flexDirection: "row", flexWrap: "wrap", gap: 4 },
  tag: { maxWidth: 120, paddingHorizontal: 6, paddingVertical: 2, borderRadius: 8, backgroundColor: colors.fill },
  tagLabel: { color: colors.muted, fontSize: 10, lineHeight: 13 },
  projectChip: {
    maxWidth: 116,
    height: 32,
    flexDirection: "row",
    alignItems: "center",
    gap: 5,
    paddingHorizontal: 9,
    borderRadius: 16,
    backgroundColor: colors.fill,
  },
  hostLabel: { color: colors.muted, fontSize: 9, fontWeight: "700" },
  projectChipLabel: { flexShrink: 1, color: colors.content, fontSize: 12, fontWeight: "600" },
  hostOption: { flexDirection: "row", alignItems: "center", gap: 10, minHeight: 48, paddingHorizontal: 12 },
  remove: { alignSelf: "flex-start", padding: 12, marginLeft: 28, minHeight: 44 },
  divider: { height: StyleSheet.hairlineWidth, backgroundColor: colors.stroke },
  overlay: { flex: 1, justifyContent: "center", padding: 20, backgroundColor: "#00000080" },
  picker: { maxHeight: "80%", padding: 16, gap: 12, borderRadius: radii.card, backgroundColor: colors.sheet },
  title: { color: colors.content, fontSize: 22, fontWeight: "600" },
  close: { alignItems: "center", justifyContent: "center", minHeight: 44 },
}));
