package com.fam.services.version.diff;

public class GoBasedDiffAnalyzer implements DiffAnalyzer {

    static {
        // 加载Go语言的动态库
        System.loadLibrary("GoHashAnalyzer");
    }

    /**
     * 调用Go语言的哈希分析模块。
     * @param filePath 文件路径
     * @return 文件的哈希值
     */
    private native String getHashFromGo(String filePath);

    @Override
    public boolean analyzeDifference(String file1Path, String file2Path) {
        // 调用Go语言的哈希分析模块获取两个文件的哈希值
        String hash1 = getHashFromGo(file1Path);
        String hash2 = getHashFromGo(file2Path);

        // 比较哈希值
        return !compareHashes(hash1, hash2);
    }
}