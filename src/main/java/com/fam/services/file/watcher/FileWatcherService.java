package com.fam.services.file.watcher;

import com.fam.services.file.scanner.FileScannerService;
import com.fam.services.file.scanner.DirectoryWalker.FileHandler;

import java.io.IOException;
import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.logging.Logger;

public class FileWatcherService {

    private static final Logger logger = Logger.getLogger(FileWatcherService.class.getName());
    private final WatchService watchService;
    private final ExecutorService executor;
    private final FileScannerService fileScannerService;
    private final FileHandler fileHandler;

    public FileWatcherService(FileScannerService fileScannerService, FileHandler fileHandler) throws IOException {
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

    public void startWatching() {
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
                        fileHandler.handle(child);
                    } else if (kind == StandardWatchEventKinds.ENTRY_MODIFY) {
                        logger.info("修改文件: " + child);
                        fileHandler.handle(child);
                    } else if (kind == StandardWatchEventKinds.ENTRY_DELETE) {
                        logger.info("删除文件: " + child);
                        // 处理删除事件
                    }
                }

                boolean valid = key.reset();
                if (!valid) {
                    break;
                }
            }
        });
    }

    public void stopWatching() {
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

            FileWatcherService fileWatcherService = new FileWatcherService(fileScannerService, fileHandler);
            fileWatcherService.registerDirectory(Paths.get("D:\\Datas\\Programs\\PGFile\\FileAutoManage(FAM)\\data"));
            fileWatcherService.startWatching();
        } catch (IOException e) {
            e.printStackTrace();
        }
    }
}