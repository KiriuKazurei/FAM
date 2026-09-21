#ifndef FAMLISTMODEL_H
#define FAMLISTMODEL_H

#include <QAbstractListModel>
#include <QStringList>
#include <QVector>
#include <functional>

#include "bridge.h"

/* A directory listing, painted from a v2 view without ever holding the whole thing.
 *
 * The core reports how many rows a directory has; this model materialises one window of them, and
 * fetches a new window only when a paint asks for a row outside the one it holds. A 78k-row folder
 * therefore costs a viewport's worth of work per frame instead of 78k row allocations, and the fetch
 * that fills a miss is a bounded copy out of the cache - no filesystem call, no wait.
 *
 * Change events arrive as positions to cut, in the order they must be applied. Consecutive equal
 * positions are a run of neighbours and become one beginRemoveRows, so deleting a thousand adjacent
 * files is one signal pair rather than a thousand.
 */
class FamListModel final : public QAbstractListModel {
    Q_OBJECT

public:
    enum Roles {
        NameRole = Qt::UserRole + 1,
        KindRole,
        SizeRole,
        ModifiedRole,
        PathRole,
        DirectoryRole,
    };

    /* Fills `window` with rows [first, first + count) of the bound directory. Returns false when the
     * core could not answer; the model then paints nothing for that row rather than lying. */
    using Fetcher = std::function<bool(quint32 first, quint32 count, FamWindow *window)>;

    explicit FamListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    void bind(quint64 token, const QString &directory, const QStringList &kinds, Fetcher fetch);
    void unbind();

    /* A finished result set that is not a directory: search hits, which the core still delivers as
     * one JSON payload. The whole list is held because there is nothing to fetch more from. */
    void loadStatic(const QVector<FamViewRow> &rows);

    /* What the core says the directory holds. Called from the status and fetch headers. */
    void readHead(const FamWindow &window);

    /* Cuts one poll reported. Returns the number of row-signal pairs emitted. */
    int applyCuts(const QVector<quint32> &positions);

    /* Rows around `row` are pulled in one fetch; the viewport is what a screenful needs. */
    void setViewport(int rows) { m_viewport = qMax(rows, 16); }

    quint32 totalRows() const { return m_total; }
    QString pathAt(int index) const;
    int count() const { return int(m_total); }

    /* Test seam: how many rows the model has materialised right now. */
    int materialised() const { return m_rows.size(); }

private:
    const FamViewRow *rowAt(int index) const;
    bool fetchAt(int row) const;

    QString m_directory;
    QStringList m_kinds;
    quint64 m_token = 0;
    Fetcher m_fetch;

    /* Painting is what triggers a fetch, so the window it fills is mutable state - rows the core
     * already served, never guesses about rows it did not. */
    mutable quint32 m_total = 0;
    mutable int m_first = -1;
    mutable QVector<FamViewRow> m_rows;
    int m_viewport = 128;
};

QString formatSize(quint64 bytes);

#endif /* FAMLISTMODEL_H */
