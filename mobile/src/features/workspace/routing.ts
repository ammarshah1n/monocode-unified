/** Desktop project IDs are encoded paths. Expo Router also decodes local route
 * params, so a navigated project can arrive as its original filesystem path.
 * Resolve it back to the catalog entry and keep its canonical ID for RPCs. */
export function projectForRoute<T extends { id: string; path: string }>(
  projects: readonly T[],
  projectId: string | undefined,
): T | undefined {
  const exact = projects.find((project) => project.id === projectId);
  if (exact) return exact;
  for (const project of projects) {
    const hosts = (project as T & { hosts?: { project: T }[] }).hosts;
    const local = hosts?.find((host) => host.project.id === projectId)?.project;
    if (local) return {
      ...project,
      ...local,
      ...("name" in project ? { name: project.name } : {}),
      hosts,
    } as T;
  }
  return projects.find((project) => project.path === projectId);
}
