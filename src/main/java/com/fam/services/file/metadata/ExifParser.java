package com.fam.services.file.metadata;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.HashMap;
import java.util.Map;

/**
 * @author Kazurei
 * @Description
 * @date 2025/3/10 2:36
 * @Project FileAutoManage(FAM)
 */

// TODO
public class ExifParser {

    private static final String GO_EXIF_PARSER_PATH = "path/to/your/go/exif/parser"; // 替换为Go程序的实际路径

    public Map<String, String> extractExif(Path filePath) {
        Map<String, String> exifData = new HashMap<>();
        ProcessBuilder processBuilder = new ProcessBuilder(GO_EXIF_PARSER_PATH, filePath.toString());

        try {
            Process process = processBuilder.start();
            BufferedReader reader = new BufferedReader(new InputStreamReader(process.getInputStream()));
            String line;
            while ((line = reader.readLine()) != null) {
                String[] parts = line.split(":", 2);
                if (parts.length == 2) {
                    exifData.put(parts[0].trim(), parts[1].trim());
                }
            }

            int exitCode = process.waitFor();
            if (exitCode != 0) {
                System.err.println("Go EXIF parser exited with error code: " + exitCode);
            }
        } catch (IOException | InterruptedException e) {
            e.printStackTrace();
        }

        return exifData;
    }

    public static void main(String[] args) {
        // 测试用方法，用于确定go程序能够正常生效
        ExifParser exifParser = new ExifParser();
        Path filePath = Paths.get("D:\\Datas\\Programs\\PGFile\\FileAutoManage(FAM)\\data\\example.jpg");
        Map<String, String> exifData = exifParser.extractExif(filePath);
        System.out.println(exifData);
    }
}