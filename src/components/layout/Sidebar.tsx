import { useEffect, useRef, useState } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import {
  Plus,
  Loader2,
  Upload,
  PackageOpen,
  Folder,
  FolderSearch,
  PanelLeftClose,
  PanelLeft,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { PlatformIcon } from "@/components/platform/PlatformIcon";
import { usePlatformStore } from "@/stores/platformStore";
import { useCollectionStore } from "@/stores/collectionStore";
import { useDiscoverStore } from "@/stores/discoverStore";
import { CollectionEditor } from "@/components/collection/CollectionEditor";
import { cn } from "@/lib/utils";

// ─── Nav Item ────────────────────────────────────────────────────────────────

function NavItem({
  label,
  isActive,
  onClick,
  icon,
  expanded,
}: {
  label: string;
  isActive: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  expanded: boolean;
}) {
  return (
    <div className="relative">
      <button
        onClick={onClick}
        title={label}
        aria-label={label}
        className={cn(
          "flex items-center w-full rounded-md transition-colors cursor-pointer",
          "hover:bg-hover-bg hover:text-white",
          isActive && "bg-primary/20 text-primary",
          expanded ? "gap-2.5 px-2.5 py-1.5 text-sm" : "justify-center py-2 px-1.5"
        )}
      >
        <span className="shrink-0">{icon}</span>
        {expanded && <span className="truncate">{label}</span>}
      </button>
      {isActive && (
        <span
          className="absolute left-0 top-1.5 bottom-1.5 w-0.5 rounded-r bg-sidebar-primary"
          aria-hidden="true"
        />
      )}
    </div>
  );
}

// ─── Sidebar ────────────────────────────────────────────────────────────────

export function Sidebar() {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const { t } = useTranslation();
  const { agents, isLoading } = usePlatformStore();

  const collections = useCollectionStore((s) => s.collections);
  const loadCollections = useCollectionStore((s) => s.loadCollections);
  const importCollection = useCollectionStore((s) => s.importCollection);

  const loadDiscoveredSkills = useDiscoverStore((s) => s.loadDiscoveredSkills);

  const [expanded, setExpanded] = useState(false);
  const [isEditorOpen, setIsEditorOpen] = useState(false);
  const importInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    loadCollections();
    loadDiscoveredSkills();
  }, [loadCollections, loadDiscoveredSkills]);

  const platformAgents = agents.filter(
    (a) => a.id !== "central" && a.is_enabled
  );

  const isCollectionActive = pathname.startsWith("/collection/");

  function handleImportClick() {
    importInputRef.current?.click();
  }

  async function handleImportFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;

    try {
      const text = await file.text();
      const collection = await importCollection(text);
      navigate(`/collection/${collection.id}`);
    } catch (err) {
      console.error("Import failed:", err);
    } finally {
      if (importInputRef.current) importInputRef.current.value = "";
    }
  }

  function handleCollectionClick() {
    if (collections.length > 0) {
      navigate(`/collection/${collections[0].id}`);
    } else {
      setIsEditorOpen(true);
    }
  }

  return (
    <nav
      className={cn(
        "flex flex-col shrink-0 h-full border-r border-border bg-sidebar text-sidebar-foreground transition-[width] duration-200",
        expanded ? "w-52" : "w-14"
      )}
      aria-label={t("sidebar.mainNav")}
    >
      {/* Toggle button */}
      <div
        className={cn(
          "flex items-center border-b border-border",
          expanded ? "justify-between px-3 py-2" : "justify-center py-2"
        )}
      >
        {expanded && (
          <span className="text-sm font-bold tracking-tight text-sidebar-primary">
            {t("app.name")}
          </span>
        )}
        <button
          onClick={() => setExpanded((e) => !e)}
          className={cn(
            "p-1 rounded-md transition-colors cursor-pointer",
            "text-muted-foreground hover:text-foreground hover:bg-muted/60"
          )}
          aria-label={expanded ? t("sidebar.collapseSidebar") : t("sidebar.expandSidebar")}
          title={expanded ? t("sidebar.collapseSidebar") : t("sidebar.expandSidebar")}
        >
          {expanded ? (
            <PanelLeftClose className="size-4" />
          ) : (
            <PanelLeft className="size-4" />
          )}
        </button>
      </div>

      {/* Scrollable nav items */}
      <div className="flex-1 overflow-y-auto py-2 px-1.5 space-y-0.5">
        {/* Central Skills */}
        <NavItem
          label={t("sidebar.centralSkills")}
          isActive={pathname === "/central" || pathname === "/"}
          onClick={() => navigate("/central")}
          icon={<PackageOpen className="size-4" />}
          expanded={expanded}
        />

        {/* Discover */}
        <NavItem
          label={t("sidebar.discovered")}
          isActive={pathname.startsWith("/discover")}
          onClick={() => navigate("/discover")}
          icon={<FolderSearch className="size-4" />}
          expanded={expanded}
        />

        {/* Collections */}
        <NavItem
          label={t("sidebar.collections")}
          isActive={isCollectionActive}
          onClick={handleCollectionClick}
          icon={<Folder className="size-4" />}
          expanded={expanded}
        />

        {/* Divider */}
        <div className="border-t border-sidebar-border/70 my-2" />

        {/* Platform icons */}
        {isLoading ? (
          <div className={cn(
            "flex items-center py-2 text-muted-foreground text-sm",
            expanded ? "gap-2 px-2.5" : "justify-center"
          )}>
            <Loader2 className="size-4 animate-spin shrink-0" />
            {expanded && <span>{t("sidebar.scanning")}</span>}
          </div>
        ) : (
          platformAgents.map((agent) => (
            <NavItem
              key={agent.id}
              label={agent.display_name}
              isActive={pathname === `/platform/${agent.id}`}
              onClick={() => navigate(`/platform/${agent.id}`)}
              icon={
                <PlatformIcon agentId={agent.id} className="size-4" />
              }
              expanded={expanded}
            />
          ))
        )}

        {/* Divider */}
        <div className="border-t border-sidebar-border/70 my-2" />

        {/* Create collection */}
        <NavItem
          label={t("sidebar.newCollectionLabel")}
          isActive={false}
          onClick={() => setIsEditorOpen(true)}
          icon={<Plus className="size-4" />}
          expanded={expanded}
        />

        {/* Import collection */}
        <NavItem
          label={t("sidebar.importCollection")}
          isActive={false}
          onClick={handleImportClick}
          icon={<Upload className="size-4" />}
          expanded={expanded}
        />
      </div>

      {/* Hidden file input for JSON import */}
      <input
        ref={importInputRef}
        type="file"
        accept=".json"
        className="hidden"
        onChange={handleImportFile}
        aria-label={t("sidebar.importCollectionInput")}
      />

      {/* Create Collection dialog */}
      <CollectionEditor
        open={isEditorOpen}
        onOpenChange={setIsEditorOpen}
        collection={null}
      />
    </nav>
  );
}
