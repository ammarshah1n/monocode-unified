import { describe, expect, it } from "vitest";
import type { Project } from "@/features/workspace/types";
import {
  chooseProjectHost,
  mergeProjectsByFolder,
  projectFolderKey,
} from "@/features/connection/hosts";

function project(id: string, name: string, path: string): Project {
  return {
    id,
    name,
    path,
    machine: "",
    branch: "main",
    color: "blue",
    additions: 0,
    deletions: 0,
  };
}

describe("projects shared between paired hosts", () => {
  it("merges case-insensitive folder basenames and keeps the first host's display name", () => {
    const [merged] = mergeProjectsByFolder([
      { fingerprint: "mac", name: "This Mac", projects: [project("mac-id", "Ops", "/Users/me/Ops/")] },
      { fingerprint: "fedora", name: "Fedora", projects: [project("fedora-id", "ops", "/home/me/ops")] },
    ]);

    expect(merged).toMatchObject({ id: "mac-id", name: "Ops", path: "/Users/me/Ops/" });
    expect(merged.hosts?.map(({ fingerprint, name, project }) => [fingerprint, name, project.id])).toEqual([
      ["mac", "This Mac", "mac-id"],
      ["fedora", "Fedora", "fedora-id"],
    ]);
    expect(projectFolderKey(merged)).toBe("ops");
  });

  it("keeps distinct basenames separate and selects the saved host or first available", () => {
    const projects = mergeProjectsByFolder([
      { fingerprint: "mac", name: "This Mac", projects: [project("mac-ops", "Ops", "/code/Ops")] },
      { fingerprint: "fedora", name: "Fedora", projects: [project("fedora-grant", "Grantmate", "/srv/Grantmate")] },
    ]);

    expect(projects).toHaveLength(2);
    expect(chooseProjectHost(projects[0], "missing")?.fingerprint).toBe("mac");
    expect(chooseProjectHost(projects[0], "fedora")?.fingerprint).toBe("mac");
    const shared = mergeProjectsByFolder([
      { fingerprint: "mac", name: "This Mac", projects: [project("a", "Ops", "/Users/me/Ops")] },
      { fingerprint: "fedora", name: "Fedora", projects: [project("b", "ops", "/home/me/ops")] },
    ])[0];
    expect(chooseProjectHost(shared, "fedora")?.fingerprint).toBe("fedora");
  });
});
