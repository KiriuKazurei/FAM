package com.fam.services.tag.storage;

import java.util.*;
import java.util.concurrent.ConcurrentHashMap;

/**
 * @author Kazurei
 * @Description 标签仓储接口实现
 * @date 2025/3/10 2:50
 * @Project FileAutoManage(FAM)
 */
public class TagRepository {
    // 使用ConcurrentHashMap来保证线程安全
    private final Map<String, Set<String>> fileTagsMap = new ConcurrentHashMap<>();

    /**
     * 添加标签到指定文件
     * @param filePath 文件路径
     * @param tags 标签集合
     */
    public void addTags(String filePath, Set<String> tags) {
        fileTagsMap.computeIfAbsent(filePath, k -> new HashSet<>()).addAll(tags);
    }

    /**
     * 删除指定文件的所有标签
     * @param filePath 文件路径
     */
    public void removeTags(String filePath) {
        fileTagsMap.remove(filePath);
    }

    /**
     * 删除指定文件的特定标签
     * @param filePath 文件路径
     * @param tags 标签集合
     */
    public void removeTags(String filePath, Set<String> tags) {
        fileTagsMap.computeIfPresent(filePath, (k, v) -> {
            v.removeAll(tags);
            return v.isEmpty() ? null : v; // 如果标签为空，移除整个条目
        });
    }

    /**
     * 获取指定文件的所有标签
     * @param filePath 文件路径
     * @return 标签集合
     */
    public Set<String> getTags(String filePath) {
        return fileTagsMap.getOrDefault(filePath, Collections.emptySet());
    }

    /**
     * 获取所有文件的标签映射
     * @return 文件路径到标签集合的映射
     */
    public Map<String, Set<String>> getAllTags() {
        return new HashMap<>(fileTagsMap);
    }

    // test
    public static void main(String[] args) {
        TagRepository tagRepository = new TagRepository();
        tagRepository.addTags("path/to/file1.jpg", Set.of("nature", "outdoor"));
        tagRepository.addTags("path/to/file2.jpg", Set.of("portrait", "indoor"));
        System.out.println(tagRepository.getTags("path/to/file1.jpg")); // 输出: [nature, outdoor]
        tagRepository.removeTags("path/to/file1.jpg", Set.of("nature"));
        System.out.println(tagRepository.getTags("path/to/file1.jpg")); // 输出: [outdoor]
        tagRepository.removeTags("path/to/file2.jpg");
        System.out.println(tagRepository.getTags("path/to/file2.jpg")); // 输出: []
    }
}