import { useCallback, useMemo } from "react";
import { useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  PackageOpen,
  FolderSearch,
  Folder,
  RefreshCw,
  Plus,
  ArrowUpRight,
} from "lucide-react";

import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { useCentralSkillsStore } from "@/stores/centralSkillsStore";
import { useDiscoverStore } from "@/stores/discoverStore";
import { useCollectionStore } from "@/stores/collectionStore";
import { usePlatformStore } from "@/stores/platformStore";
import { useHotkey } from "@/hooks/useHotkey";
import { PlatformIcon } from "@/components/platform/PlatformIcon";

interface GlobalSearchDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onAction: (action: string) => void;
}

type SearchItem = {
  id: string;
  label: string;
  description?: string;
  group: string;
  icon: React.ReactNode;
  onSelect: () => void;
};

export function GlobalSearchDialog({
  open,
  onOpenChange,
  onAction,
}: GlobalSearchDialogProps) {
  const navigate = useNavigate();
  const { t } = useTranslation();

  // Data sources
  const centralSkills = useCentralSkillsStore((s) => s.skills);
  const discoveredProjects = useDiscoverStore((s) => s.discoveredProjects);
  const collections = useCollectionStore((s) => s.collections);
  const agents = usePlatformStore((s) => s.agents);

  const close = useCallback(() => onOpenChange(false), [onOpenChange]);

  // Build flat search items
  const items = useMemo<SearchItem[]>(() => {
    const result: SearchItem[] = [];

    // Central Skills
    for (const skill of centralSkills) {
      result.push({
        id: `central-${skill.id}`,
        label: skill.name,
        description: skill.description,
        group: t("globalSearch.centralSkills"),
        icon: <PackageOpen className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          navigate(`/skill/${skill.id}`);
        },
      });
    }

    // Discovered Skills
    const discoveredSkills = discoveredProjects.flatMap((p) => p.skills);
    for (const skill of discoveredSkills) {
      result.push({
        id: `discovered-${skill.id}`,
        label: skill.name,
        description: `${skill.project_name} / ${skill.platform_name}`,
        group: t("globalSearch.discovered"),
        icon: <FolderSearch className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          navigate("/discover");
        },
      });
    }

    // Collections
    for (const col of collections) {
      result.push({
        id: `collection-${col.id}`,
        label: col.name,
        description: col.description,
        group: t("globalSearch.collections"),
        icon: <Folder className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          navigate(`/collection/${col.id}`);
        },
      });
    }

    // Platform Views
    const platformAgents = agents.filter(
      (a) => a.id !== "central" && a.is_enabled
    );
    for (const agent of platformAgents) {
      result.push({
        id: `platform-${agent.id}`,
        label: agent.display_name,
        description: agent.global_skills_dir,
        group: t("globalSearch.platforms"),
        icon: (
          <PlatformIcon agentId={agent.id} className="size-4 text-primary/70" />
        ),
        onSelect: () => {
          close();
          navigate(`/platform/${agent.id}`);
        },
      });
    }

    // Actions
    result.push(
      {
        id: "action-rescan",
        label: t("globalSearch.actionRescan"),
        group: t("globalSearch.actions"),
        icon: <RefreshCw className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          onAction("rescan");
        },
      },
      {
        id: "action-new-collection",
        label: t("globalSearch.actionNewCollection"),
        group: t("globalSearch.actions"),
        icon: <Plus className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          onAction("new-collection");
        },
      },
      {
        id: "action-discover",
        label: t("globalSearch.actionDiscover"),
        group: t("globalSearch.actions"),
        icon: <ArrowUpRight className="size-4 shrink-0 text-primary/70" />,
        onSelect: () => {
          close();
          navigate("/discover");
        },
      }
    );

    return result;
  }, [
    centralSkills,
    discoveredProjects,
    collections,
    agents,
    navigate,
    close,
    onAction,
    t,
  ]);

  // Group names in order
  const groups = useMemo(() => {
    const seen = new Set<string>();
    const order: string[] = [];
    for (const item of items) {
      if (!seen.has(item.group)) {
        seen.add(item.group);
        order.push(item.group);
      }
    }
    return order;
  }, [items]);

  // Cmd+K shortcut (also registered here so the dialog self-toggles)
  useHotkey("mod+k", () => onOpenChange(!open));

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("globalSearch.title")}
      description={t("globalSearch.description")}
      className="sm:max-w-lg"
      showCloseButton={false}
    >
      <Command>
        <CommandInput
          placeholder={t("globalSearch.placeholder")}
        />
        <CommandList>
          <CommandEmpty>{t("globalSearch.noResults")}</CommandEmpty>
          {groups.map((group) => (
            <CommandGroup key={group} heading={group}>
              {items
                .filter((item) => item.group === group)
                .map((item) => (
                  <CommandItem
                    key={item.id}
                    value={`${item.label} ${item.description ?? ""}`}
                    onSelect={item.onSelect}
                  >
                    {item.icon}
                    <div className="flex flex-col min-w-0">
                      <span className="truncate text-sm">{item.label}</span>
                      {item.description && (
                        <span className="truncate text-xs text-muted-foreground">
                          {item.description}
                        </span>
                      )}
                    </div>
                  </CommandItem>
                ))}
            </CommandGroup>
          ))}
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
