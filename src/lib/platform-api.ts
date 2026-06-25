import type {
  HomeApiActivityItem,
  HomeApiOpenOrderItem,
  HomeApiPositionItem,
  HomeOverview as DesktopHomeOverview,
} from "./tauri-commands";

export type ProfilePerformanceRange = "6h" | "1d" | "7d" | "30d" | "all";

export type ProfilePerformancePoint = {
  ts: string;
  value: number;
  raw_value: number;
};

export type ProfilePerformanceWindow = {
  range: ProfilePerformanceRange;
  label: string;
  profit_loss: number | null;
  series: ProfilePerformancePoint[];
};

export type ProfilePerformanceDailyStat = {
  date: string;
  pnl: number | null;
  volume: number | null;
  maker_rebate: number | null;
  lp_rewards: number | null;
  trades: number;
};

export type ProfilePerformanceView = {
  ok: boolean;
  profile_id: string | null;
  profile_name: string | null;
  range: ProfilePerformanceRange;
  profit_loss: number | null;
  realized_pnl: number | null;
  open_pnl: number | null;
  position_value: number | null;
  available_balance: number | null;
  rewards: number | null;
  series: ProfilePerformancePoint[];
  windows: Record<ProfilePerformanceRange, ProfilePerformanceWindow>;
  daily_stats: ProfilePerformanceDailyStat[];
  daily_stats_partial?: boolean;
  all_time: {
    profit_loss: number | null;
    volume: number | null;
    maker_rebate: number | null;
    lp_rewards: number | null;
    volume_partial?: boolean;
    maker_rebate_partial?: boolean;
  } | null;
  as_of_utc: string;
  source: string;
  error: string | null;
};

export type HomeOverview = DesktopHomeOverview;
export type HomeActivityItem = HomeApiActivityItem;
export type HomePositionItem = HomeApiPositionItem;
export type HomeOpenOrderItem = HomeApiOpenOrderItem;
