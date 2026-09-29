// 共享查询 hooks：统一 query key 与轮询节奏（近实时：0.5s 轮询 + 0.5s flush，感知延迟 ≤1s）。
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import * as client from "./client";
import type { Range } from "./types";

/** 今日/总览数据轮询间隔（近实时；数据 0.5s flush 落库，感知延迟 ≤1s） */
export const POLL_TODAY_MS = 500;

export function useOverview(range: Range) {
  return useQuery({
    queryKey: ["overview", range.from, range.to],
    queryFn: () => client.getOverview(range.from, range.to),
    placeholderData: keepPreviousData,
    refetchInterval: POLL_TODAY_MS,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });
}

export function useCollectorStatus() {
  return useQuery({
    queryKey: ["collectorStatus"],
    queryFn: () => client.collectorStatus(),
    refetchInterval: POLL_TODAY_MS,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });
}

export function useDevices() {
  return useQuery({
    queryKey: ["devices"],
    queryFn: () => client.getDevices(),
    refetchInterval: 2_000,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });
}

export function useSettings() {
  return useQuery({ queryKey: ["settings"], queryFn: () => client.getSettings() });
}
