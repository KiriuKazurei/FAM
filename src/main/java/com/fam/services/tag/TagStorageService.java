package com.fam.services.tag;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

public class TagStorageService {
    private Map<String, List<String>> fileTags = new HashMap<>();

    public void addTags(String filePath, List<String> tags) {
        fileTags.put(filePath, tags);
    }

    public List<String> getTags(String filePath) {
        return fileTags.getOrDefault(filePath, List.of());
    }

    public void removeTags(String filePath) {
        fileTags.remove(filePath);
    }

    public void updateTags(String filePath, List<String> newTags) {
        fileTags.put(filePath, newTags);
    }
}