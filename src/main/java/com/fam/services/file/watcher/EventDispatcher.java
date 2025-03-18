package com.fam.services.file.watcher;

/**
 * @author Kazurei
 * @Description
 * @date 2025/3/10 2:24
 * @Project FileAutoManage(FAM)
 **/
import com.fam.services.file.scanner.DirectoryWalker;
import com.fam.services.file.scanner.FileHandler;
import com.fam.services.file.scanner.FileScannerService;

import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.logging.Logger;

public class EventDispatcher {

    private static final Logger logger = Logger.getLogger(EventDispatcher.class.getName());
    private final WatchService watchService;
    private final ExecutorService executor;
    private final FileScannerService fileScannerService;
    private final FileHandler fileHandler;

    public EventDispatcher(FileScannerService fileScannerService, FileHandler fileHandler) throws IOException {
        this.fileScannerService = fileScannerService;
        this.fileHandler = fileHandler;
        this.watchService = FileSystems.getDefault().newWatchService();
        this.executor = Executors.newSingleThreadExecutor();
    }

    public void registerDirectory(Path dir) throws IOException {
        dir.register(watchService, StandardWatchEventKinds.ENTRY_CREATE,
                StandardWatchEventKinds.ENTRY_MODIFY,
                StandardWatchEventKinds.ENTRY_DELETE);
    }

    public void startDispatching() {
        executor.submit(() -> {
            while (true) {
                WatchKey key;
                try {
                    key = watchService.take();
                } catch (InterruptedException x) {
                    return;
                }

                for (WatchEvent<?> event : key.pollEvents()) {
                    WatchEvent.Kind<?> kind = event.kind();

                    if (kind == StandardWatchEventKinds.OVERFLOW) {
                        continue;
                    }

                    WatchEvent<Path> ev = (WatchEvent<Path>) event;
                    Path filename = ev.context();

                    Path child = ((Path) key.watchable()).resolve(filename);

                    if (kind == StandardWatchEventKinds.ENTRY_CREATE) {
                        logger.info("创建文件: " + child);
                        handleEvent(child, kind);
                    } else if (kind == StandardWatchEventKinds.ENTRY_MODIFY) {
                        logger.info("修改文件: " + child);
                        handleEvent(child, kind);
                    } else if (kind == StandardWatchEventKinds.ENTRY_DELETE) {
                        logger.info("删除文件: " + child);
                        handleEvent(child, kind);
                    }
                }

                boolean valid = key.reset();
                if (!valid) {
                    break;
                }
            }
        });
    }

    private void handleEvent(Path file, WatchEvent.Kind<?> kind) {
        if (fileScannerService.isAIAvailable()) {
            // 处理AI逻辑
            // ...
        }
        fileHandler.handle(file);
    }

    public void stopDispatching() {
        executor.shutdown();
        try {
            if (!executor.awaitTermination(60, java.util.concurrent.TimeUnit.SECONDS)) {
                executor.shutdownNow();
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            executor.shutdownNow();
        }
    }

    public static void main(String[] args) {
        try {
            FileScannerService fileScannerService = new FileScannerService();
            FileHandler fileHandler = file -> {
                // 处理文件逻辑
                System.out.println("处理文件: " + file);
            };

            EventDispatcher eventDispatcher = new EventDispatcher(fileScannerService, fileHandler);
            eventDispatcher.registerDirectory(Paths.get("D:\\Datas\\Programs\\PGFile\\FileAutoManage(FAM)\\data"));
            eventDispatcher.startDispatching();
        } catch (IOException e) {
            e.printStackTrace();
        }
    }
}
