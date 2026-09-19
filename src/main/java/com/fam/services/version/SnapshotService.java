package com.fam.services.version;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.util.UUID;

public class SnapshotService {
    private String snapshotDirectory;

    public SnapshotService(String snapshotDirectory) {
        this.snapshotDirectory = snapshotDirectory;
        new File(snapshotDirectory).mkdirs();
    }

    public String createSnapshot(String filePath) throws IOException {
        Path source = Paths.get(filePath);
        String snapshotId = UUID.randomUUID().toString();
        Path destination = Paths.get(snapshotDirectory, snapshotId);

        Files.copy(source, destination, StandardCopyOption.REPLACE_EXISTING);
        return snapshotId;
    }

    public void restoreSnapshot(String snapshotId, String targetPath) throws IOException {
        Path source = Paths.get(snapshotDirectory, snapshotId);
        Path destination = Paths.get(targetPath);

        Files.copy(source, destination, StandardCopyOption.REPLACE_EXISTING);
    }
}