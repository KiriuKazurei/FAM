#ifndef ICONCACHE_H
#define ICONCACHE_H

#include <QHash>
#include <QIcon>
#include <QImage>
#include <QMutex>
#include <QObject>
#include <QString>
#include <QThread>
#include <QVector>
#include <QWaitCondition>
#include <atomic>

/* What one cached icon is keyed by.
 *
 * Two tiers, chosen by the caller from data a row already carries - never by opening a path:
 *
 * - tier 1 is the extension plus the raw file attributes. It resolves through the shell's own
 *   system image list with SHGFI_USEFILEATTRIBUTES, which is the flag that makes SHGetFileInfoW
 *   answer about a *name shape* rather than a file: the path string is never opened. That is the
 *   whole reason a 78k-row folder can have an icon per row - the naive per-row SHGetFileInfoW on a
 *   real path is what freezes file browsers, and on a network namespace it blocks forever.
 * - tier 2 is a real path, and only for the handful of names whose icon is not in their extension:
 *   links (`.lnk`, `.url`, `.library-ms`), drive roots, and directories, whose `desktop.ini` can
 *   rename their icon. Those cost one path lookup each and are resolved for the viewport only.
 */
struct IconKey {
    QString suffix; /* ".txt" lower-cased, empty when the name has no extension */
    QString path;   /* tier 2 only: the real path the icon must be read from */
    quint32 attrs = 0; /* raw dwFileAttributes off the wire record */
    bool directory = false;
    bool tier2 = false;

    bool operator==(const IconKey &other) const {
        return suffix == other.suffix && path == other.path && attrs == other.attrs &&
               directory == other.directory && tier2 == other.tier2;
    }
};

inline uint qHash(const IconKey &key, uint seed = 0) {
    return ::qHash(key.suffix, seed) ^ ::qHash(key.path, seed * 31 + 7) ^
           ::qHash(quint32(key.attrs), seed * 131 + 17) ^
           (key.directory ? 0x9e3779b9u : 0u) ^ (key.tier2 ? 0x85ebca6bu : 0u);
}

/* One resolved key coming back from the worker thread. The image, not an icon: QImage is the one
 * pixel container Qt allows to be built off the GUI thread, and an HICON never crosses this
 * boundary at all - it is destroyed where it was extracted. */
struct IconResult {
    IconKey key;
    QImage image;
};

Q_DECLARE_METATYPE(QVector<IconResult>)

/* The one thread allowed to talk to the shell's image lists.
 *
 * STA because the shell namespace is COM, and dedicated because the first SHGetImageList builds the
 * system image list - a 100ms-to-seconds stall on a machine with many shell handlers registered -
 * which must never land on the paint path. It warms all three list sizes at start so a later view
 * that wants a bigger icon does not pay for the build mid-scroll.
 */
class IconWorker final : public QThread {
    Q_OBJECT

public:
    explicit IconWorker(QObject *parent = nullptr);
    ~IconWorker() override;

    void request(const QVector<IconKey> &keys);
    void stop();

    /* HICONs extracted versus destroyed. Equal in a healthy process; the self-check logs both after
     * a scroll storm because a leaked icon is the classic Windows-browser bug and it is invisible
     * until the desktop runs out of handles. */
    int extracted() const { return m_extracted.load(); }
    int destroyed() const { return m_destroyed.load(); }
    /* Keys still waiting to be resolved. The self-check reads this beside the two counters above,
     * because "extracted equals destroyed" only means something when nothing is in flight: a worker
     * caught mid-resolve legitimately has one more extracted than destroyed with nothing leaked. */
    int pending() const {
        QMutexLocker locker(&m_lock);
        return m_pending.size();
    }

signals:
    void resolved(const QVector<IconResult> &results);

protected:
    void run() override;

private:
    mutable QMutex m_lock;
    QWaitCondition m_wake;
    QVector<IconKey> m_pending;
    bool m_stop = false;
    std::atomic<int> m_extracted{0};
    std::atomic<int> m_destroyed{0};
};

/* The GUI-thread face of the icon layer: hand it a key while painting, get an icon or a placeholder
 * back immediately, and hear `resolved` when the worker has pixels for keys it was asked about.
 *
 * The cache holds QIcons, not rows: a model asks per row but stores nothing per row, so an icon is
 * one hash entry per extension rather than one allocation per file. `resolved` carries no row
 * numbers on purpose - which rows to repaint is the model's business, because only it knows what is
 * on screen.
 */
class IconCache final : public QObject {
    Q_OBJECT

public:
    explicit IconCache(QObject *parent = nullptr);
    ~IconCache() override;

    /* The icon for `key` right now: the resolved one when the worker has been here before, the
     * bucket placeholder otherwise, with a request queued behind the scenes. Never blocks, never
     * touches the filesystem. */
    QIcon iconFor(const IconKey &key);

    int extracted() const { return m_worker.extracted(); }
    int destroyed() const { return m_worker.destroyed(); }
    int pending() const { return m_worker.pending(); }
    int cached() const;

signals:
    /* A batch of keys gained pixels. Queued onto the GUI thread by the worker's signal. */
    void resolved();

private slots:
    void store(const QVector<IconResult> &results);

private:
    IconWorker m_worker;
    QHash<IconKey, QIcon> m_icons;
    QHash<IconKey, bool> m_requested;
    QIcon m_filePlaceholder;
    QIcon m_dirPlaceholder;
};

/* Builds the key a row's icon is looked up by, from what the row already carries. */
IconKey iconKeyFor(const QString &name, const QString &path, quint32 attrs, bool directory);

#endif /* ICONCACHE_H */
