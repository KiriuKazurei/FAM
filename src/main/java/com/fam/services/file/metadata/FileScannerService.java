package com.fam.services.file.metadata;

import java.io.File;
import java.util.ArrayList;
import java.util.List;

public class FileScannerService {
    public List<String> scanDirectory(String directoryPath) {
        List<String> imageFiles = new ArrayList<>();
        File directory = new File(directoryPath);
        
        if (directory.exists() && directory.isDirectory()) {
            File[] files = directory.listFiles();
            if (files != null) {
                for (File file : files) {
                    if (isImageFile(file.getName())) {
                        imageFiles.add(file.getAbsolutePath());
                    }
                }
            }
        }
        
        return imageFiles;
    }

    private boolean isImageFile(String fileName) {
        String[] validExtensions = {".jpg", ".jpeg", ".png", ".gif", ".bmp", ".tiff", ".cr2", ".nef", ".arw"};
        for (String ext : validExtensions) {
            if (fileName.toLowerCase().endsWith(ext)) {
                return true;
            }
        }
        return false;
    }
}