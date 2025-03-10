package com.fam.services.file.scanner;

import java.io.IOException;
import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.concurrent.*;
import java.util.function.Predicate;
import java.util.logging.Logger;

/**
 * 高效目录遍历器，支持多线程扫描和文件过滤
 *
 * @see FileScannerService
 */
public class DirectoryWalker {

    private static final Logger logger = Logger.getLogger(DirectoryWalker.class.getName());
    private static final int LOWEND_THREAD_POOL = 2; // 低配设备线程数
    private static final int MAX_FILE_QUEUE_SIZE = 1024;

    private final ExecutorService executor;
    private final BlockingQueue<Path> fileQueue = new ArrayBlockingQueue<>(MAX_FILE_QUEUE_SIZE);
    private final FileScannerService fileScannerService;

    /**
     * 构造函数
     * @param fileScannerService 文件扫描服务
     */
    public DirectoryWalker(FileScannerService fileScannerService) {
        this.fileScannerService = fileScannerService;
        int threadCount = fileScannerService.isAIAvailable() ? Runtime.getRuntime().availableProcessors() : LOWEND_THREAD_POOL;
        this.executor = Executors.newFixedThreadPool(threadCount);
    }

    /**
     * 同步扫描指定目录
     * @param rootPath 根目录路径
     * @param filter 文件过滤器
     * @param handler 文件处理器
     */
    public void scanDirectory(Path rootPath, Predicate<Path> filter, FileHandler handler) {
        try {
            Files.walkFileTree(rootPath, new SimpleFileVisitor<>() {
                @Override
                public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) {
                    if (filter.test(file)) {
                        fileQueue.offer(file);
                    }
                    return FileVisitResult.CONTINUE;
                }
            });
        } catch (IOException e) {
            logger.severe("目录遍历失败: " + e.getMessage());
        } finally {
            shutdown();
        }

        processFiles(handler);
    }

    private void processFiles(FileHandler handler) {
        executor.submit(() -> {
            while (!fileQueue.isEmpty()) {
                Path file = fileQueue.poll();
                if (file != null) {
                    if (fileScannerService.isAIAvailable()) {
                        // 处理AI逻辑
                        // ...
                    }
                    handler.handle(file);
                }
            }
        });
    }

    /**
     * 安全关闭线程池
     */
    public void shutdown() {
        executor.shutdown();
        try {
            if (!executor.awaitTermination(60, TimeUnit.SECONDS)) {
                executor.shutdownNow();
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            executor.shutdownNow();
        }
    }

    // -------------------------------------------------------------------------------
    // 接口定义
    // -------------------------------------------------------------------------------

    @FunctionalInterface
    public interface FileHandler {
        void handle(Path file);
    }

    // -------------------------------------------------------------------------------
    // 配置建议
    // -------------------------------------------------------------------------------

    /**
     * 推荐配置参数（根据硬件动态调整）
     */
    public static class Config {
        public static final int LOWEND_THREAD_POOL = 2; // 低配设备线程数
        public static final int MAX_CONCURRENT_FILES = 256; // 最大并发处理文件数
        public static final Predicate<Path> DEFAULT_FILTER = path -> 
            path.toString().endsWith(".jpg") ||
            path.toString().endsWith(".raw") ||
            path.toString().endsWith(".png");
    }
}