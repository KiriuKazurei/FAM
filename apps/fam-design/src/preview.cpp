#include "preview.h"

#include <QAction>
#include <QApplication>
#include <QComboBox>
#include <QDir>
#include <QFileDialog>
#include <QFileInfo>
#include <QFontMetrics>
#include <QHBoxLayout>
#include <QLabel>
#include <QItemSelectionModel>
#include <QLineEdit>
#include <QListView>
#include <QListWidget>
#include <QPainter>
#include <QPen>
#include <QPixmap>
#include <QPushButton>
#include <QSignalBlocker>
#include <QStackedWidget>
#include <QVBoxLayout>

#include "previewmodel.h"
#include "rowdelegate.h"
#include "theme.h"

namespace {

/* The nine buckets `fam_core::Kind` declares, in its order. */
constexpr int kKindCount = 9;

QString sortWord(const QString &key) {
    if (key == QLatin1String("size")) {
        return QStringLiteral("Size");
    }
    if (key == QLatin1String("time")) {
        return QStringLiteral("Modified");
    }
    return QStringLiteral("Name");
}

/* The folder's own name; a volume root has no file name, and the letter is what a person calls it. */
QString heading(const QString &path) {
    const QString name = QFileInfo(path).fileName();
    return name.isEmpty() ? QDir::toNativeSeparators(path) : name;
}

} /* namespace */

PreviewWindow::PreviewWindow(const QString &startDirectory, QWidget *parent) : QWidget(parent) {
    const Tokens &tokens = Theme::instance().tokens();
    setObjectName(Theme::pageName());
    m_shown.resize(kColumns);
    m_shown[0] = startDirectory;

    m_search = new QLineEdit(this);
    m_search->setObjectName(Theme::searchName());
    m_search->setPlaceholderText(QStringLiteral("在这个文件夹里找名字…"));
    m_search->setClearButtonEnabled(true);

    m_kind = new QComboBox(this);
    m_kind->addItem(QStringLiteral("任何类型"), -1);
    for (int kind = 0; kind < kKindCount; ++kind) {
        m_kind->addItem(preview::kindName(kind), kind);
    }

    m_sort = new QComboBox(this);
    m_sort->addItem(QStringLiteral("按名称"), QStringLiteral("name"));
    m_sort->addItem(QStringLiteral("按大小"), QStringLiteral("size"));
    m_sort->addItem(QStringLiteral("按时间"), QStringLiteral("time"));

    auto *search = new QPushButton(QStringLiteral("搜索"), this);
    search->setObjectName(Theme::primaryButtonName());
    auto *open = new QPushButton(QStringLiteral("打开文件夹…"), this);
    m_themeButton = new QPushButton(QStringLiteral("暗色"), this);
    m_themeButton->setObjectName(Theme::iconButtonName());
    m_themeButton->setToolTip(QStringLiteral("在亮色与暗色之间切换"));
    m_themeButton->setText(Theme::instance().scheme() == Scheme::Dark ? QStringLiteral("亮色")
                                                                     : QStringLiteral("暗色"));

    auto *toolbar = new QWidget(this);
    toolbar->setObjectName(Theme::toolbarName());
    auto *bar = new QHBoxLayout(toolbar);
    bar->setContentsMargins(tokens.lg, tokens.sm, tokens.lg, tokens.sm);
    bar->setSpacing(tokens.sm);
    bar->addWidget(m_search, 1);
    bar->addWidget(m_kind);
    bar->addWidget(m_sort);
    bar->addWidget(search);
    bar->addWidget(open);
    bar->addWidget(m_themeButton);

    m_rail = new QListWidget(this);
    m_rail->setObjectName(Theme::sidebarName());
    m_rail->setFrameShape(QFrame::NoFrame);
    m_rail->setMaximumWidth(220);
    for (const QFileInfo &drive : QDir::drives()) {
        const QString root = drive.absoluteFilePath();
        auto *item = new QListWidgetItem();
        item->setText(heading(root));
        item->setToolTip(root);
        item->setData(Qt::UserRole, root);
        m_rail->addItem(item);
    }

    auto *columns = new QHBoxLayout;
    columns->setSpacing(tokens.xs);
    for (int index = 0; index < kColumns; ++index) {
        columns->addWidget(buildColumn(index), 1);
    }
    auto *content = new QWidget(this);
    auto *contentBox = new QHBoxLayout(content);
    contentBox->setContentsMargins(tokens.md, tokens.md, tokens.lg, tokens.md);
    contentBox->addLayout(columns, 1);

    m_status = new QLabel(this);
    m_status->setObjectName(Theme::statusName());
    m_status->setContentsMargins(tokens.lg, tokens.sm, tokens.lg, tokens.sm);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    layout->addWidget(toolbar);

    auto *body = new QHBoxLayout;
    body->setContentsMargins(0, 0, 0, 0);
    body->setSpacing(0);
    body->addWidget(m_rail);
    body->addWidget(content, 1);
    layout->addLayout(body, 1);
    layout->addWidget(m_status);

    connect(search, &QPushButton::clicked, this, &PreviewWindow::refresh);
    connect(m_search, &QLineEdit::returnPressed, this, &PreviewWindow::refresh);
    /* Emptying the box is "stop filtering", not "search for nothing" - the same reading the product
     * makes, and the reason the box answers textChanged rather than only the button. */
    connect(m_search, &QLineEdit::textChanged, this, &PreviewWindow::refresh);
    connect(m_kind, &QComboBox::currentIndexChanged, this, &PreviewWindow::refresh);
    connect(m_sort, &QComboBox::currentIndexChanged, this, [this] {
        refresh();
        updateTitles();
    });
    connect(open, &QPushButton::clicked, this, &PreviewWindow::browseForFolder);
    connect(m_themeButton, &QPushButton::clicked, this, &PreviewWindow::toggleScheme);
    connect(m_rail, &QListWidget::currentRowChanged, this, [this](int row) {
        if (QListWidgetItem *item = m_rail->item(row)) {
            descend(0, item->data(Qt::UserRole).toString());
        }
    });
    for (int index = 0; index < kColumns; ++index) {
        QListView *view = m_views[index];
        QPushButton *header = m_headers[index];
        QObject::connect(view->selectionModel(), &QItemSelectionModel::currentChanged, this,
                         [this, index, view](const QModelIndex &current, const QModelIndex &) {
                             if (!current.isValid()) {
                                 return;
                             }
                             updateStatus();
                             if (index + 1 >= kColumns ||
                                 !m_models[index]->isDirectory(current.row())) {
                                 return;
                             }
                             /* The row that was just picked becomes the next column - which is the
                              * whole of a Miller chain's interaction, and the reason the two views
                              * share one handler. */
                             descend(index + 1,
                                     current.data(PreviewModel::PathRole).toString());
                         });
        QObject::connect(header, &QPushButton::clicked, this, [this, index] {
            /* The header is the sort control, and it cycles the order the same select the toolbar
             * shows is set to - one of the two has to follow the other, and it is this one. */
            const int next = (m_sort->currentIndex() + 1) % m_sort->count();
            m_sort->setCurrentIndex(next);
        });
    }

    applySearchGlyph();
    refresh();
    /* One folder to start with, so the second column is on screen and the design can be looked at
     * with a chain rather than a single pane. A seed, not a rule: it runs here once, and from the
     * first click onwards the chain belongs to the click. */
    if (m_shown[1].isEmpty()) {
        for (int row = 0; row < m_models[0]->rowCount(); ++row) {
            if (m_models[0]->isDirectory(row)) {
                descend(1, m_models[0]->pathAt(row));
                break;
            }
        }
    }
    updateTitles();

    /* Mark the volume the window opened on, but with the signal blocked: marking a row is also the
     * gesture that means "show me this drive", and letting it fire here would quietly throw away the
     * folder the caller asked for. */
    {
        const QSignalBlocker held(m_rail);
        for (int row = 0; row < m_rail->count(); ++row) {
            /* Both sides normalised: `--dir` arrives with backslashes on Windows and the drive list
             * spells its roots with slashes, and a comparison that forgets that marks the wrong
             * volume - which looks like a selection bug and is really a spelling bug. */
            const QString root = QDir::fromNativeSeparators(
                                     m_rail->item(row)->data(Qt::UserRole).toString())
                                     .toUpper();
            const QString shown = QDir::fromNativeSeparators(m_shown[0]).toUpper();
            if (!root.isEmpty() && shown.startsWith(root)) {
                m_rail->setCurrentRow(row);
                break;
            }
        }
    }
}

QWidget *PreviewWindow::buildColumn(int index) {
    const Tokens &tokens = Theme::instance().tokens();
    auto *card = new QWidget(this);
    card->setObjectName(Theme::columnFrameName());

    auto *header = new QWidget(card);
    header->setObjectName(Theme::columnHeaderName());
    auto *title = new QLabel(header);
    title->setObjectName(Theme::pathName());
    auto *sort = new QPushButton(header);
    sort->setObjectName(QString::fromLatin1(Theme::sortHeaderName()));
    sort->setFlat(true);
    sort->setCursor(Qt::PointingHandCursor);
    sort->setToolTip(QStringLiteral("切换这一列的排序"));
    auto *strip = new QHBoxLayout(header);
    strip->setContentsMargins(0, 0, 0, 0);
    strip->setSpacing(0);
    strip->addWidget(title, 1);
    strip->addWidget(sort, 0, Qt::AlignVCenter);

    auto *model = new PreviewModel(card);
    auto *view = new QListView(card);
    view->setObjectName(Theme::listName());
    view->setModel(model);
    view->setItemDelegate(new RowDelegate(&Theme::instance(),
                                               {PreviewModel::NameRole, PreviewModel::KindRole,
                                                PreviewModel::SizeRole, PreviewModel::DirectoryRole,
                                                &preview::formatSize},
                                               view));
    /* Hover is a row state the delegate reads, and a view only tracks it when asked. */
    view->setMouseTracking(true);
    view->setFrameShape(QFrame::NoFrame);
    view->setUniformItemSizes(true);
    view->setSelectionBehavior(QAbstractItemView::SelectRows);
    view->setSelectionMode(QAbstractItemView::SingleSelection);
    view->setResizeMode(QListView::Adjust);
    view->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);

    auto *empty = new QLabel(card);
    empty->setObjectName(Theme::emptyName());
    empty->setAlignment(Qt::AlignCenter);
    auto *pages = new QStackedWidget(card);
    pages->addWidget(view);
    pages->addWidget(empty);

    auto *box = new QVBoxLayout(card);
    box->setContentsMargins(0, 0, 0, 0);
    box->setSpacing(0);
    box->addWidget(header);
    box->addWidget(pages, 1);
    card->setMinimumWidth(180);

    m_cards.append(card);
    m_titles.append(title);
    m_headers.append(sort);
    m_views.append(view);
    m_models.append(model);
    m_pages.append(pages);
    return card;
}

void PreviewWindow::descend(int level, const QString &path) {
    if (path.isEmpty()) {
        return;
    }
    for (int index = level; index < kColumns; ++index) {
        m_shown[index] = index == level ? path : QString();
    }
    refresh();
    updateTitles();
}

void PreviewWindow::refresh() {
    const QString needle = m_search->text().trimmed();
    const int kind = m_kind->currentData().toInt();
    const QString order = m_sort->currentData().toString();
    for (int index = 0; index < kColumns; ++index) {
        const QString path = m_shown.value(index);
        PreviewModel *model = m_models[index];
        QWidget *card = m_cards[index];
        if (path.isEmpty()) {
            model->load(QString());
            card->hide();
            continue;
        }
        const int rows = model->load(path, needle, kind, order);
        card->show();
        /* "Nothing matched" and "this folder cannot be read" are opposite claims, and a design that
         * renders them with the same sentence is the design the product already had to fix once. */
        if (rows == 0) {
            auto *empty = qobject_cast<QLabel *>(m_pages[index]->widget(1));
            empty->setText(model->isReadable() ? QStringLiteral("这里没有匹配的项目")
                                               : QStringLiteral("打不开这个文件夹"));
        }
        m_pages[index]->setCurrentIndex(rows == 0 ? 1 : 0);
    }
    updateStatus();
}

void PreviewWindow::updateTitles() {
    const QString order = m_sort->currentData().toString();
    const QString arrow = QStringLiteral(" ▲");
    for (int index = 0; index < kColumns; ++index) {
        m_titles[index]->setText(heading(m_shown.value(index)));
        m_titles[index]->setToolTip(m_shown.value(index));
        m_headers[index]->setText(sortWord(order) + arrow);
    }
}

void PreviewWindow::updateStatus() {
    /* The sentence is about the deepest folder on screen, because that is the one the reader is
     * looking at. The selection is theirs, wherever they made it. */
    int deepest = 0;
    QString selected;
    for (int index = 0; index < kColumns; ++index) {
        if (!m_shown.value(index).isEmpty()) {
            deepest = index;
        }
        const QModelIndex current = m_views[index]->currentIndex();
        if (current.isValid()) {
            selected = current.data(PreviewModel::PathRole).toString();
        }
    }
    PreviewModel *model = m_models[deepest];
    const int rows = model->rowCount();
    int folders = 0;
    for (int row = 0; row < rows; ++row) {
        folders += model->isDirectory(row) ? 1 : 0;
    }
    QString detail = QStringLiteral("%1 项 · %2 个文件夹").arg(rows).arg(folders);
    if (!selected.isEmpty()) {
        const QFileInfo info(selected);
        detail += QStringLiteral(" · 选中 %1").arg(info.fileName());
        if (!info.isDir()) {
            detail += QStringLiteral(" · %1 · 改于 %2")
                          .arg(preview::formatSize(static_cast<quint64>(info.size())),
                               preview::formatWhen(info.lastModified().toMSecsSinceEpoch()));
        }
    }
    /* The tightest gated contrast pair, on the line, next to the scheme name: the claim that this
     * design is readable is a measurement, and it belongs where someone would think to check it. */
    detail += QStringLiteral(" · 最紧的一处 %1:1 · %2")
                  .arg(Theme::instance().tightestRatio(), 0, 'f', 2)
                  .arg(Theme::instance().name());
    m_status->setText(detail);
}

void PreviewWindow::applySearchGlyph() {
    const Tokens &tokens = Theme::instance().tokens();
    QPixmap glyph(16, 16);
    glyph.fill(Qt::transparent);
    QPainter painter(&glyph);
    painter.setRenderHint(QPainter::Antialiasing, true);
    QPen pen(tokens.textTertiary);
    pen.setWidthF(1.4);
    pen.setCapStyle(Qt::RoundCap);
    painter.setPen(pen);
    painter.setBrush(Qt::NoBrush);
    painter.drawEllipse(QRectF(2.4, 2.4, 7.0, 7.0));
    painter.drawLine(QPointF(9.3, 9.3), QPointF(13.2, 13.2));
    painter.end();

    if (m_searchGlyph == nullptr) {
        m_searchGlyph = m_search->addAction(glyph, QLineEdit::LeadingPosition);
        return;
    }
    m_searchGlyph->setIcon(glyph);
}

void PreviewWindow::toggleScheme() {
    Theme &theme = Theme::instance();
    theme.setScheme(Theme::opposite(theme.scheme()));
    qApp->setFont(theme.applicationFont());
    qApp->setStyleSheet(theme.styleSheet());
    /* Three things the style sheet cannot reach by itself: a glyph it has no rule for, the rows a
     * delegate paints, and this button's own word for what it does next. */
    applySearchGlyph();
    m_themeButton->setText(theme.scheme() == Scheme::Dark ? QStringLiteral("亮色")
                                                          : QStringLiteral("暗色"));
    for (QListView *view : m_views) {
        view->viewport()->update();
    }
    updateStatus();
}

void PreviewWindow::browseForFolder() {
    const QString picked = QFileDialog::getExistingDirectory(
        this, QStringLiteral("打开文件夹"), m_shown.value(0).isEmpty() ? QDir::homePath()
                                                                       : m_shown.value(0));
    if (!picked.isEmpty()) {
        descend(0, picked);
    }
}
