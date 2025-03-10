package com.fam.services.file.scanner;

import java.lang.management.ManagementFactory;
import java.lang.management.OperatingSystemMXBean;
import java.lang.reflect.Method;

public class FileScannerService {

    private boolean isAIAvailable;

    public FileScannerService() {
        this.isAIAvailable = checkHardwareCapability();
    }

    private boolean checkHardwareCapability() {
        // 内存检测
        OperatingSystemMXBean osBean = ManagementFactory.getOperatingSystemMXBean();
        long totalPhysicalMemory = 0;
        try {
            Method method = osBean.getClass().getMethod("getTotalPhysicalMemorySize");
            totalPhysicalMemory = (Long) method.invoke(osBean);
        } catch (Exception e) {
            e.printStackTrace();
        }

        // 转换为MB
        long totalPhysicalMemoryMB = totalPhysicalMemory / (1024 * 1024);

        // CPU线程数检测
        int availableProcessors = Runtime.getRuntime().availableProcessors();

        // 指令集检测（这里简化为检查是否支持AVX2，实际需要更复杂的检测）
        boolean hasAVX2 = checkForAVX2();

        return (totalPhysicalMemoryMB >= 8192) && (availableProcessors >= 4) && hasAVX2;
    }

    private boolean checkForAVX2() {
        // 简化实现，实际需要通过CPUID指令检测
        return true; // 假设支持AVX2
    }

    public boolean isAIAvailable() {
        return isAIAvailable;
    }

}